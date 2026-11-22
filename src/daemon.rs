//! The long-running process: socket, scheduler, trigger polls, config reload, status snapshot.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use chrono::{DateTime, Local};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};

use crate::app::{App, Turn};
use crate::config::DaemonConfig;
use crate::ipc;
use crate::paths::Paths;
use crate::schedule::Schedule;
use crate::store::{NewItem, Status, Store};
use crate::{triggers, worker};

pub async fn run(paths: Paths) -> anyhow::Result<()> {
    paths.ensure()?;
    let config = DaemonConfig::load(&paths)?;
    let store = Store::open(&paths.db()).context("open state.db")?;
    let exe = std::env::current_exe()?;
    let app = App::new(paths.clone(), config, store, exe);

    recover(&app);
    ipc::reload(&app);
    worker::resume_running(&app);

    let socket = paths.socket();
    let server = tokio::spawn({
        let app = Arc::clone(&app);
        async move { ipc::serve(app, &socket).await }
    });
    tokio::spawn(scheduler(Arc::clone(&app)));
    tokio::spawn(triggers::poll_loop(Arc::clone(&app)));
    tokio::spawn(watch_config(Arc::clone(&app)));
    tokio::spawn(snapshot_writer(Arc::clone(&app)));
    app.changed.notify_one();
    info!(version = env!("CARGO_PKG_VERSION"), root = %paths.root().display(), "herdr-bots daemon up");

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        r = server => r??,
        _ = term.recv() => info!("SIGTERM"),
        _ = int.recv() => info!("SIGINT"),
    }
    // Bots and workers live in hc terminals and keep running; next start re-attaches.
    let _ = std::fs::remove_file(paths.socket());
    let _ = app.write_snapshot();
    Ok(())
}

/// After a restart, items that were mid-turn go back in the queue, told what happened.
/// Items waiting on workers stay working; `resume_running` re-attaches to those workers.
fn recover(app: &App) {
    let store = app.store();
    for item in store.items_with_status(Status::Working).unwrap_or_default() {
        if store.running_workers_for(item.id).unwrap_or(0) > 0 {
            continue;
        }
        let _ = store.transition(item.id, Status::Planned, "daemon restarted");
        let _ = store.event(item.id, "requeued", "daemon restarted mid-turn");
    }
}

/// Sleeps until the next schedule entry is due, files it, repeats. Wakes early when bots change.
async fn scheduler(app: Arc<App>) {
    loop {
        let next = next_due(&app);
        // Capped at a minute: tokio's clock is monotonic and can stop while the Mac sleeps, so
        // re-check the wall clock often instead of trusting one long sleep.
        let sleep = match &next {
            Some((at, ..)) => (*at - Local::now()).to_std().unwrap_or(Duration::ZERO),
            None => Duration::from_secs(60),
        }
        .min(Duration::from_secs(60));
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            _ = app.bots_changed.notified() => continue,
        }
        let Some((at, bot, index, prompt)) = next else {
            continue;
        };
        if Local::now() < at {
            continue; // woke early (clock change); recompute
        }
        let key = format!("schedule:{index}:{}", at.to_rfc3339());
        let title: String = prompt
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect();
        let new = NewItem {
            bot: &bot,
            source: "schedule",
            external_id: Some(&key),
            title: &title,
            body: &prompt,
            url: None,
        };
        match app.store().add_item(&new) {
            Ok(Some(item)) => {
                info!(bot, item = item.id, "scheduled");
                app.enqueue(
                    &bot,
                    Turn {
                        item: item.id,
                        note: None,
                    },
                );
            }
            Ok(None) => {}
            Err(e) => warn!(bot, "schedule: {e}"),
        }
    }
}

fn next_due(app: &App) -> Option<(DateTime<Local>, String, usize, String)> {
    app.bots()
        .into_iter()
        .flat_map(|bot| {
            bot.schedule
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let at = Schedule::parse(&e.when).ok()?.next_local()?;
                    Some((at, bot.id.clone(), i, e.prompt.clone()))
                })
                .collect::<Vec<_>>()
        })
        .min_by_key(|(at, ..)| *at)
}

/// Reloads when a bot folder or bot.toml appears, disappears or changes. Polls every few seconds:
/// cheap (one `stat` per bot) and works the same on macOS and Linux.
async fn watch_config(app: Arc<App>) {
    let mut last = fingerprint(&app.paths);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let now = fingerprint(&app.paths);
        if now != last {
            info!("bot config changed; reloading");
            ipc::reload(&app);
            last = now;
        }
    }
}

fn fingerprint(paths: &Paths) -> BTreeMap<String, Option<SystemTime>> {
    let mut map = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(paths.bots_dir()) {
        for e in entries.flatten() {
            let mtime = std::fs::metadata(e.path().join("bot.toml"))
                .and_then(|m| m.modified())
                .ok();
            map.insert(e.file_name().to_string_lossy().into_owned(), mtime);
        }
    }
    map
}

/// Coalesces bursts of changes into one status.json write at most every 250 ms.
async fn snapshot_writer(app: Arc<App>) {
    loop {
        app.changed.notified().await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Err(e) = app.write_snapshot() {
            warn!("status.json: {e}");
        }
    }
}
