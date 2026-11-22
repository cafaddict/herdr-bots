//! One actor per bot: takes turns off its queue one at a time, so a bot never has two
//! conversations going in the same terminal.

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::agent::{Readiness, TurnEnd, agent_argv, drive_turn, ensure_agent};
use crate::app::{App, Turn, terminal_name};
use crate::store::{Item, Status};
use crate::templates;

/// Starts the bot's actor if it has none, and queues its planned items.
pub fn ensure_actor(app: &Arc<App>, bot: &str) {
    if app.has_actor(bot) {
        return;
    }
    let (tx, rx) = mpsc::unbounded_channel();
    app.register_actor(bot, tx);
    tokio::spawn(actor(Arc::clone(app), bot.to_owned(), rx));
    requeue_planned(app, bot);
}

pub fn requeue_planned(app: &App, bot: &str) {
    let planned = app
        .store()
        .items_with_status(Status::Planned)
        .unwrap_or_default();
    for item in planned.into_iter().filter(|i| i.bot == bot) {
        app.enqueue(
            bot,
            Turn {
                item: item.id,
                note: None,
            },
        );
    }
}

async fn actor(app: Arc<App>, bot: String, mut rx: mpsc::UnboundedReceiver<Turn>) {
    while let Some(turn) = rx.recv().await {
        if let Err(e) = run_turn(&app, &bot, &turn).await {
            warn!(bot, item = turn.item, "turn failed: {e:#}");
            let store = app.store();
            if let Ok(item) = store.item(turn.item)
                && item.status == Status::Working
            {
                let _ = store.ask(
                    turn.item,
                    &format!("herdr-bots couldn't run this turn: {e:#}"),
                );
            }
        }
        app.changed.notify_one();
    }
}

async fn run_turn(app: &App, bot_id: &str, turn: &Turn) -> anyhow::Result<()> {
    let Some(bot) = app.bot(bot_id) else {
        return Ok(());
    };
    let item = {
        let store = app.store();
        if store.paused(bot_id)? {
            return Ok(()); // stays planned; `resume` queues it again
        }
        let item = store.item(turn.item)?;
        match item.status {
            Status::Planned => store.transition(item.id, Status::Working, "turn")?,
            // A continuation (a worker reported back) on an item that's still open.
            Status::Working if turn.note.is_some() => item,
            _ => return Ok(()), // answered twice, canceled meanwhile, ...
        }
    };
    app.changed.notify_one();
    info!(bot = bot_id, item = item.id, "turn");

    let name = terminal_name(bot_id);
    let home = app.paths.bot_home(bot_id);
    std::fs::create_dir_all(home.join("outbox"))?;
    let argv = agent_argv(bot.agent, bot.permission, bot.model.as_deref());
    let readiness = ensure_agent(&app.hc, &name, &home, &[("herdr-bot", bot_id)], &argv).await?;
    if readiness == Readiness::Blocked {
        app.store().ask(
            item.id,
            "The bot's agent is waiting on a first-start dialog (folder trust or permissions). \
             Open the bot from the panel, answer it, then reply here to continue.",
        )?;
        return Ok(());
    }

    let outbox = app.paths.outbox(bot_id, item.id);
    let _ = std::fs::remove_file(&outbox); // only this turn's answer counts
    let end = drive_turn(
        &app.hc,
        &name,
        &templates::turn_message(&item, turn.note.as_deref()),
    )
    .await?;
    finish_turn(app, &item, &name, &end, &outbox).await
}

async fn finish_turn(
    app: &App,
    item: &Item,
    terminal: &str,
    end: &TurnEnd,
    outbox: &std::path::Path,
) -> anyhow::Result<()> {
    let reply = match std::fs::read_to_string(outbox) {
        Ok(text) if !text.trim().is_empty() => Some(text),
        _ => None,
    };
    let reply = match reply {
        Some(r) => r,
        None => {
            let tail = app.hc.read(terminal, 40).await.unwrap_or_default();
            format!(
                "(no outbox/{}.md; last lines of the terminal)\n\n{}",
                item.id,
                tail.trim_end()
            )
        }
    };
    let store = app.store();
    store.set_reply(item.id, &reply)?;
    let current = store.item(item.id)?;
    if current.status != Status::Working {
        return Ok(()); // the bot asked a question (needs-you) or someone canceled it
    }
    match end {
        TurnEnd::Finished if store.running_workers_for(item.id)? > 0 => {} // workers will report back
        TurnEnd::Finished => {
            store.transition(item.id, Status::Done, "turn finished")?;
        }
        TurnEnd::Blocked => {
            store.ask(item.id, "The bot is waiting for a permission answer in its terminal. Open it from the panel.")?;
        }
        TurnEnd::Exited(code) => {
            store.ask(
                item.id,
                &format!("The bot's agent exited (code {code:?}). Reply to start it again."),
            )?;
        }
        TurnEnd::Failed => {
            store.ask(
                item.id,
                "The bot's agent reported an error. Check its terminal, then reply to retry.",
            )?;
        }
    }
    Ok(())
}
