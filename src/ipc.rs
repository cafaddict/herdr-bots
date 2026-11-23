//! The daemon's control socket: one JSON request per line, one JSON response per line.
//!
//! The CLI is the only client (the herdr mod calls the CLI), so the protocol can stay private.
//! The socket is 0600 inside the user's own folder: only the user can reach it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::app::{App, Turn, terminal_name};
use crate::store::{NewItem, Status};
use crate::{config, runner, worker};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Ping,
    Status,
    /// Message a bot. With `item`, answers that needs_you item instead of filing a new one.
    Ask {
        bot: String,
        text: String,
        item: Option<i64>,
    },
    NeedsYou {
        item: i64,
        question: String,
    },
    Done {
        item: i64,
    },
    Cancel {
        item: i64,
    },
    Pause {
        bot: String,
    },
    Resume {
        bot: String,
    },
    Reload,
    WorkerStart {
        item: i64,
        repo: PathBuf,
        spec: String,
        base: Option<String>,
    },
    WorkerDone {
        name: String,
        pr: Option<String>,
        summary: String,
    },
    /// Deletes a bot: cancels its open items, ends its terminal, moves its folder to trash/.
    /// Refuses while its workers run, unless `force` (then they're stopped too).
    Remove {
        bot: String,
        #[serde(default)]
        force: bool,
    },
    /// Renames a bot: its folder, its items, workers and trigger state. Ends its terminal (the
    /// next turn starts a new session under the new name). Refuses while it is on a turn or
    /// its workers run.
    Rename {
        bot: String,
        to: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn serve(app: Arc<App>, socket: &Path) -> anyhow::Result<()> {
    if socket.exists() {
        // A live daemon answers; a stale socket from a crash doesn't.
        if UnixStream::connect(socket).await.is_ok() {
            anyhow::bail!(
                "another daemon is already listening on {}",
                socket.display()
            );
        }
        std::fs::remove_file(socket)?;
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind {}", socket.display()))?;
    restrict(socket)?;
    info!("listening on {}", socket.display());
    loop {
        let (stream, _) = listener.accept().await?;
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            if let Err(e) = connection(app, stream).await {
                warn!("ipc: {e:#}");
            }
        });
    }
}

#[cfg(unix)]
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

async fn connection(app: Arc<App>, stream: UnixStream) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => match handle(&app, req).await {
                Ok(data) => Response {
                    ok: true,
                    data,
                    error: None,
                },
                Err(e) => Response {
                    ok: false,
                    data: Value::Null,
                    error: Some(format!("{e:#}")),
                },
            },
            Err(e) => Response {
                ok: false,
                data: Value::Null,
                error: Some(format!("bad request: {e}")),
            },
        };
        let mut out = serde_json::to_vec(&response)?;
        out.push(b'\n');
        write.write_all(&out).await?;
        app.changed.notify_one();
    }
    Ok(())
}

async fn handle(app: &Arc<App>, req: Request) -> anyhow::Result<Value> {
    match req {
        Request::Ping => Ok(json!({ "pid": std::process::id() })),
        Request::Status => Ok(app.snapshot()),
        Request::Ask { bot, text, item } => {
            anyhow::ensure!(app.bot(&bot).is_some(), "no bot `{bot}`");
            let item = match item {
                Some(id) => {
                    let current = app.store().item(id)?;
                    anyhow::ensure!(current.bot == bot, "item {id} belongs to {}", current.bot);
                    app.store().answer(id, &text)?
                }
                None => {
                    let title: String = text
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(80)
                        .collect();
                    let new = NewItem {
                        bot: &bot,
                        source: "chat",
                        external_id: None,
                        title: &title,
                        body: &text,
                        url: None,
                    };
                    app.store().add_item(&new)?.context("item was not filed")?
                }
            };
            app.enqueue(
                &bot,
                Turn {
                    item: item.id,
                    note: None,
                },
            );
            Ok(serde_json::to_value(item)?)
        }
        Request::NeedsYou { item, question } => {
            Ok(serde_json::to_value(app.store().ask(item, &question)?)?)
        }
        Request::Done { item } => Ok(serde_json::to_value(app.store().transition(
            item,
            Status::Done,
            "by hand",
        )?)?),
        Request::Cancel { item } => Ok(serde_json::to_value(app.store().transition(
            item,
            Status::Canceled,
            "by hand",
        )?)?),
        Request::Pause { bot } => {
            app.store().set_paused(&bot, true)?;
            // Stop the current turn too; its item goes to needs_you when the turn returns.
            let _ = app.hc.interrupt(&terminal_name(&bot)).await;
            Ok(json!({ "paused": bot }))
        }
        Request::Resume { bot } => {
            app.store().set_paused(&bot, false)?;
            runner::requeue_planned(app, &bot);
            Ok(json!({ "resumed": bot }))
        }
        Request::Reload => {
            reload(app);
            Ok(json!({ "bots": app.bots().len() }))
        }
        Request::WorkerStart {
            item,
            repo,
            spec,
            base,
        } => {
            let w = worker::start(
                app,
                worker::StartRequest {
                    item,
                    repo,
                    spec,
                    base,
                },
            )
            .await?;
            Ok(serde_json::to_value(w)?)
        }
        Request::WorkerDone { name, pr, summary } => {
            let pr = pr.filter(|p| p != "-");
            let w = app
                .store()
                .report_worker(&name, pr.as_deref(), &summary)?
                .with_context(|| format!("no worker {name}"))?;
            Ok(serde_json::to_value(w)?)
        }
        Request::Remove { bot, force } => remove_bot(app, &bot, force).await,
        Request::Rename { bot, to } => rename_bot(app, &bot, &to).await,
    }
}

async fn rename_bot(app: &Arc<App>, bot: &str, to: &str) -> anyhow::Result<Value> {
    anyhow::ensure!(config::valid_bot_id(bot), "no bot `{bot}`");
    anyhow::ensure!(
        config::valid_bot_id(to),
        "`{to}` is not a valid bot id (1-24 of a-z 0-9 - _)"
    );
    anyhow::ensure!(bot != to, "{bot} already has that name");
    let home = app.paths.bot_home(bot);
    let dest = app.paths.bot_home(to);
    anyhow::ensure!(home.is_dir(), "no bot `{bot}`");
    anyhow::ensure!(!dest.exists(), "a bot `{to}` already exists");
    // Hold the bot first, then check: `run_turn` reads the pause under the store lock before it
    // starts a turn, so from here no new turn begins, and the check can't go stale under us.
    let was_paused = app.store().paused(bot)?;
    app.store().set_paused(bot, true)?;
    app.remove_actor(bot);
    if let Err(e) = rename_blockers(app, bot) {
        undo_hold(app, bot, was_paused);
        return Err(e);
    }

    // End its session: the terminal is named after the bot.
    let _ = app.hc.kill(&terminal_name(bot)).await;
    let _ = app.hc.rm(&terminal_name(bot)).await;

    // The folder first (one rename), then the rows; if the rows fail, the folder goes back.
    if let Err(e) = std::fs::rename(&home, &dest) {
        undo_hold(app, bot, was_paused);
        return Err(anyhow::Error::new(e).context(format!(
            "move {} to {}",
            home.display(),
            dest.display()
        )));
    }
    if let Err(e) = app.store().rename_bot(bot, to, &stamp()) {
        let _ = std::fs::rename(&dest, &home);
        undo_hold(app, bot, was_paused);
        reload(app);
        return Err(e.into());
    }
    app.store().set_paused(to, was_paused)?;
    info!(bot, to, "bot renamed");
    reload(app);
    runner::requeue_planned(app, to);
    Ok(json!({ "renamed": bot, "to": to, "home": dest }))
}

/// Why `bot` can't be renamed right now: workers running, or a turn under way.
fn rename_blockers(app: &App, bot: &str) -> anyhow::Result<()> {
    let store = app.store();
    let running: Vec<String> = store
        .workers(true, 1000)?
        .into_iter()
        .filter(|w| w.bot == bot)
        .map(|w| w.name)
        .collect();
    anyhow::ensure!(
        running.is_empty(),
        "{bot} has running workers ({}); rename it after they finish",
        running.join(", ")
    );
    let busy = store
        .items_with_status(Status::Working)?
        .into_iter()
        .any(|i| i.bot == bot);
    anyhow::ensure!(!busy, "{bot} is on a turn; rename it after the turn ends");
    Ok(())
}

/// Gives a bot back its queue after a rename that didn't happen.
fn undo_hold(app: &Arc<App>, bot: &str, was_paused: bool) {
    if !was_paused {
        let _ = app.store().set_paused(bot, false);
    }
    runner::ensure_actor(app, bot);
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

async fn remove_bot(app: &Arc<App>, bot: &str, force: bool) -> anyhow::Result<Value> {
    anyhow::ensure!(config::valid_bot_id(bot), "no bot `{bot}`");
    let home = app.paths.bot_home(bot);
    anyhow::ensure!(home.is_dir(), "no bot `{bot}`");

    let running: Vec<String> = app
        .store()
        .workers(true, 1000)?
        .into_iter()
        .filter(|w| w.bot == bot)
        .map(|w| w.name)
        .collect();
    anyhow::ensure!(
        running.is_empty() || force,
        "{bot} has running workers ({}); stop them first or use --force",
        running.join(", ")
    );

    // Stop new turns first, then cancel what's open, then end the processes.
    app.remove_actor(bot);
    let mut canceled = 0;
    {
        let store = app.store();
        for status in [Status::Planned, Status::Working, Status::NeedsYou] {
            for item in store
                .items_with_status(status)?
                .into_iter()
                .filter(|i| i.bot == bot)
            {
                store.transition(item.id, Status::Canceled, "bot removed")?;
                canceled += 1;
            }
        }
        for name in &running {
            store.finish_worker(name, "failed", None, Some("stopped: its bot was removed"))?;
        }
    }
    for terminal in running.iter().cloned().chain([terminal_name(bot)]) {
        let _ = app.hc.kill(&terminal).await;
        let _ = app.hc.rm(&terminal).await;
    }

    // Kept, not deleted: SOUL.md and MEMORY.md are the bot's history.
    let trash = app.paths.root().join("trash");
    std::fs::create_dir_all(&trash)?;
    let tag = stamp();
    let dest = trash.join(format!("{bot}-{tag}"));
    std::fs::rename(&home, &dest)
        .with_context(|| format!("move {} to {}", home.display(), dest.display()))?;
    // Free the name: a new bot called `bot` starts without this one's pause, seen triggers or items.
    app.store().retire_bot(bot, &tag)?;
    info!(bot, trash = %dest.display(), canceled, "bot removed");
    reload(app);
    Ok(
        json!({ "removed": bot, "trash": dest, "canceled_items": canceled, "stopped_workers": running }),
    )
}

/// Re-reads every bot.toml, rewrites each bot's HERDR-BOTS.md, and starts actors for new bots.
pub fn reload(app: &Arc<App>) {
    let (bots, errors) = config::load_bots(&app.paths);
    for (id, err) in &errors {
        warn!(bot = id, "config: {err}");
    }
    for id in bots.keys() {
        let rules = crate::templates::rules_md(id, &app.exe);
        let path = app.paths.bot_home(id).join("HERDR-BOTS.md");
        if std::fs::read_to_string(&path).ok().as_deref() != Some(rules.as_str())
            && let Err(e) = std::fs::write(&path, rules)
        {
            warn!(bot = id, "write rules: {e}");
        }
    }
    let ids: Vec<String> = bots.keys().cloned().collect();
    app.set_bots(bots, errors);
    for id in ids {
        runner::ensure_actor(app, &id);
    }
}

/// Client side: send one request, get its response.
pub async fn call(socket: &Path, req: &Request) -> anyhow::Result<Value> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("herdr-bots daemon is not running ({})", socket.display()))?;
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    let mut lines = BufReader::new(read).lines();
    let reply = lines
        .next_line()
        .await?
        .context("daemon closed the connection")?;
    let resp: Response = serde_json::from_str(&reply)?;
    if resp.ok {
        Ok(resp.data)
    } else {
        anyhow::bail!(resp.error.unwrap_or_else(|| "unknown error".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let req: Request = serde_json::from_str(r#"{"cmd":"ask","bot":"a","text":"hi"}"#).unwrap();
        assert!(matches!(req, Request::Ask { item: None, .. }));
        let line = serde_json::to_string(&Request::WorkerDone {
            name: "w1-1".into(),
            pr: None,
            summary: "s".into(),
        })
        .unwrap();
        assert!(line.contains(r#""cmd":"worker-done""#));
    }
}
