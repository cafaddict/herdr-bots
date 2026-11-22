//! State the daemon's tasks share.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::config::{BotConfig, DaemonConfig};
use crate::hc::Hc;
use crate::paths::Paths;
use crate::schedule::Schedule;
use crate::store::{Status, Store};

/// One unit of work for a bot's actor: run a turn on this item, optionally with an update to tell
/// the bot (a worker finished, the daemon restarted).
#[derive(Debug, Clone)]
pub struct Turn {
    pub item: i64,
    pub note: Option<String>,
}

pub struct App {
    pub paths: Paths,
    pub config: DaemonConfig,
    pub hc: Hc,
    /// This program, for the paths written into bots' rules.
    pub exe: PathBuf,
    store: Mutex<Store>,
    bots: RwLock<BTreeMap<String, BotConfig>>,
    bot_errors: RwLock<BTreeMap<String, String>>,
    actors: Mutex<HashMap<String, mpsc::UnboundedSender<Turn>>>,
    /// Pinged on every change; the snapshot writer and the scheduler listen.
    pub changed: Notify,
    pub bots_changed: Notify,
    /// GitHub login of whoever `gh` is signed in as (the default allow list).
    pub me: RwLock<Option<String>>,
}

impl App {
    pub fn new(paths: Paths, config: DaemonConfig, store: Store, exe: PathBuf) -> Arc<Self> {
        let hc = Hc::new(config.hc.clone());
        Arc::new(Self {
            paths,
            config,
            hc,
            exe,
            store: Mutex::new(store),
            bots: RwLock::default(),
            bot_errors: RwLock::default(),
            actors: Mutex::default(),
            changed: Notify::new(),
            bots_changed: Notify::new(),
            me: RwLock::default(),
        })
    }

    /// Short, synchronous access. Never hold it across an `.await`.
    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn bot(&self, id: &str) -> Option<BotConfig> {
        self.bots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    pub fn bots(&self) -> Vec<BotConfig> {
        self.bots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    pub fn set_bots(&self, bots: BTreeMap<String, BotConfig>, errors: BTreeMap<String, String>) {
        *self
            .bots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = bots;
        *self
            .bot_errors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = errors;
        self.bots_changed.notify_waiters();
        self.changed.notify_one();
    }

    pub fn register_actor(&self, bot: &str, tx: mpsc::UnboundedSender<Turn>) {
        self.actors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(bot.into(), tx);
    }

    pub fn has_actor(&self, bot: &str) -> bool {
        self.actors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(bot)
    }

    /// Queues a turn for the item's bot. Dropped silently if the bot is gone (its item stays
    /// planned and is picked up when the bot comes back).
    pub fn enqueue(&self, bot: &str, turn: Turn) {
        if let Some(tx) = self
            .actors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(bot)
        {
            let _ = tx.send(turn);
        }
        self.changed.notify_one();
    }

    /// Who may trigger `bot` from outside: its own list, else the daemon's, else me.
    pub fn allow_list(&self, bot: &BotConfig) -> Vec<String> {
        if !bot.allow_authors.is_empty() {
            return bot.allow_authors.clone();
        }
        if !self.config.allow_authors.is_empty() {
            return self.config.allow_authors.clone();
        }
        self.me
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Everything the panel shows, as one JSON document.
    pub fn snapshot(&self) -> Value {
        let store = self.store();
        let items = store.recent_items(30).unwrap_or_default();
        let workers = store.workers(false, 30).unwrap_or_default();
        let bots: Vec<Value> = self
            .bots()
            .iter()
            .map(|b| {
                let mine = items.iter().filter(|i| i.bot == b.id);
                let count = |s: Status| mine.clone().filter(|i| i.status == s).count();
                let (needs, working, planned) = (count(Status::NeedsYou), count(Status::Working), count(Status::Planned));
                let state = if needs > 0 {
                    "needs_you"
                } else if working > 0 {
                    "working"
                } else if planned > 0 {
                    "planned"
                } else {
                    "idle"
                };
                let schedule: Vec<Value> = b
                    .schedule
                    .iter()
                    .map(|e| {
                        let next = Schedule::parse(&e.when).ok().and_then(|s| s.next_local());
                        json!({ "when": e.when, "prompt": e.prompt, "next": next.map(|t| t.to_rfc3339()) })
                    })
                    .collect();
                json!({
                    "id": b.id,
                    "agent": b.agent,
                    "permission": b.permission,
                    "terminal": terminal_name(&b.id),
                    "home": self.paths.bot_home(&b.id),
                    "paused": store.paused(&b.id).unwrap_or(false),
                    "state": state,
                    "needs_you": needs,
                    "working": working,
                    "planned": planned,
                    "schedule": schedule,
                    "github": b.github.is_some(),
                    "linear": b.linear.is_some(),
                })
            })
            .collect();
        let errors = self
            .bot_errors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "updated_at": chrono::Utc::now().timestamp_millis(),
            "bots": bots,
            "bot_errors": errors,
            "items": items,
            "workers": workers,
        })
    }

    /// Writes the snapshot atomically (tmp + rename) so the panel never reads half a file.
    pub fn write_snapshot(&self) -> std::io::Result<()> {
        let path = self.paths.status_json();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.snapshot())?)?;
        std::fs::rename(tmp, path)
    }
}

pub fn terminal_name(bot: &str) -> String {
    format!("bot-{bot}")
}
