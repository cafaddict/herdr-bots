//! The work list: items (one job for one bot), their history, workers, and trigger cursors.
//!
//! SQLite in WAL mode. Every status change goes through [`Store::transition`], which refuses
//! moves the state machine doesn't allow, so a bug elsewhere can't quietly reopen a finished item.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Planned,
    Working,
    NeedsYou,
    Done,
    Canceled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Planned => "planned",
            Status::Working => "working",
            Status::NeedsYou => "needs_you",
            Status::Done => "done",
            Status::Canceled => "canceled",
        }
    }

    fn parse(s: &str) -> rusqlite::Result<Self> {
        Ok(match s {
            "planned" => Status::Planned,
            "working" => Status::Working,
            "needs_you" => Status::NeedsYou,
            "done" => Status::Done,
            "canceled" => Status::Canceled,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown status {other}").into(),
                ));
            }
        })
    }

    pub fn is_final(self) -> bool {
        matches!(self, Status::Done | Status::Canceled)
    }

    /// The state machine. `Working -> Planned` is the requeue after a daemon restart.
    pub fn can_become(self, next: Status) -> bool {
        use Status::*;
        matches!(
            (self, next),
            (Planned, Working | Canceled)
                | (Working, Done | NeedsYou | Canceled | Planned)
                | (NeedsYou, Planned | Done | Canceled)
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    pub bot: String,
    pub source: String,
    pub external_id: Option<String>,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub status: Status,
    pub question: Option<String>,
    pub reply: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Worker {
    pub name: String,
    pub item_id: i64,
    pub bot: String,
    pub repo: String,
    pub worktree: String,
    pub branch: String,
    pub status: String,
    pub pr_url: Option<String>,
    pub summary: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewItem<'a> {
    pub bot: &'a str,
    pub source: &'a str,
    /// Dedupe key for triggers: the same (bot, source, external_id) is only ever filed once.
    pub external_id: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
    pub url: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("item {0} not found")]
    NotFound(i64),
    #[error("item {id} is {from}, it can't become {to}")]
    BadTransition {
        id: i64,
        from: &'static str,
        to: &'static str,
    },
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS items (
    id          INTEGER PRIMARY KEY,
    bot         TEXT NOT NULL,
    source      TEXT NOT NULL,             -- chat | schedule | github | linear
    external_id TEXT,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    url         TEXT,
    status      TEXT NOT NULL CHECK (status IN ('planned','working','needs_you','done','canceled')),
    question    TEXT,                      -- what the bot asks me while needs_you
    reply       TEXT,                      -- the bot's last answer (outbox)
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    UNIQUE (bot, source, external_id)
);
CREATE INDEX IF NOT EXISTS items_by_status ON items (status, bot);
CREATE TABLE IF NOT EXISTS item_events (
    id      INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL REFERENCES items (id),
    kind    TEXT NOT NULL,
    data    TEXT NOT NULL,
    at      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS workers (
    name       TEXT PRIMARY KEY,
    item_id    INTEGER NOT NULL REFERENCES items (id),
    bot        TEXT NOT NULL,
    repo       TEXT NOT NULL,
    worktree   TEXT NOT NULL,
    branch     TEXT NOT NULL,
    status     TEXT NOT NULL CHECK (status IN ('running','done','failed')),
    pr_url     TEXT,
    summary    TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS bot_state (
    bot    TEXT PRIMARY KEY,
    paused INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS seen (
    bot TEXT NOT NULL,
    key TEXT NOT NULL,
    at  INTEGER NOT NULL,
    PRIMARY KEY (bot, key)
);
CREATE TABLE IF NOT EXISTS cursors (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

const ITEM_COLUMNS: &str = "id, bot, source, external_id, title, body, url, status, question, reply, created_at, updated_at";

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        bot: row.get(1)?,
        source: row.get(2)?,
        external_id: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        url: row.get(6)?,
        status: Status::parse(&row.get::<_, String>(7)?)?,
        question: row.get(8)?,
        reply: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn retire(tx: &rusqlite::Transaction<'_>, bot: &str, tag: &str) -> Result<(), StoreError> {
    let archived = format!("{bot}~{tag}");
    for table in ["items", "workers"] {
        tx.execute(
            &format!("UPDATE {table} SET bot = ?2 WHERE bot = ?1"),
            params![bot, archived],
        )?;
    }
    tx.execute("DELETE FROM bot_state WHERE bot = ?1", [bot])?;
    tx.execute("DELETE FROM seen WHERE bot = ?1", [bot])?;
    tx.execute(
        "DELETE FROM cursors WHERE substr(key, 1, length('baseline:' || ?1 || ':')) = 'baseline:' || ?1 || ':'",
        [bot],
    )?;
    Ok(())
}

fn row_to_worker(row: &rusqlite::Row<'_>) -> rusqlite::Result<Worker> {
    Ok(Worker {
        name: row.get(0)?,
        item_id: row.get(1)?,
        bot: row.get(2)?,
        repo: row.get(3)?,
        worktree: row.get(4)?,
        branch: row.get(5)?,
        status: row.get(6)?,
        pr_url: row.get(7)?,
        summary: row.get(8)?,
        created_at: row.get(9)?,
    })
}

const WORKER_COLUMNS: &str =
    "name, item_id, bot, repo, worktree, branch, status, pr_url, summary, created_at";

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Files a new item, or returns `None` when a trigger already filed this one.
    pub fn add_item(&self, new: &NewItem<'_>) -> Result<Option<Item>, StoreError> {
        let at = now();
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO items (bot, source, external_id, title, body, url, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'planned', ?7, ?7)",
            params![new.bot, new.source, new.external_id, new.title, new.body, new.url, at],
        )?;
        if inserted == 0 {
            return Ok(None);
        }
        let id = self.conn.last_insert_rowid();
        self.event(id, "filed", new.source)?;
        self.item(id).map(Some)
    }

    pub fn item(&self, id: i64) -> Result<Item, StoreError> {
        self.conn
            .query_row(
                &format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1"),
                [id],
                row_to_item,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn transition(&self, id: i64, to: Status, note: &str) -> Result<Item, StoreError> {
        let item = self.item(id)?;
        if !item.status.can_become(to) {
            return Err(StoreError::BadTransition {
                id,
                from: item.status.as_str(),
                to: to.as_str(),
            });
        }
        self.conn.execute(
            "UPDATE items SET status = ?2, updated_at = ?3,
                 question = CASE WHEN ?2 = 'needs_you' THEN question ELSE NULL END
             WHERE id = ?1",
            params![id, to.as_str(), now()],
        )?;
        self.event(id, to.as_str(), note)?;
        self.item(id)
    }

    /// Moves to needs_you with the question the bot is asking.
    pub fn ask(&self, id: i64, question: &str) -> Result<Item, StoreError> {
        self.transition(id, Status::NeedsYou, question)?;
        self.conn.execute(
            "UPDATE items SET question = ?2 WHERE id = ?1",
            params![id, question],
        )?;
        self.item(id)
    }

    /// My answer to a needs_you item: appended to its body, and back in the queue.
    pub fn answer(&self, id: i64, answer: &str) -> Result<Item, StoreError> {
        let item = self.transition(id, Status::Planned, answer)?;
        let body = format!("{}\n\n[answer] {answer}", item.body);
        self.conn.execute(
            "UPDATE items SET body = ?2 WHERE id = ?1",
            params![id, body],
        )?;
        self.item(id)
    }

    pub fn set_reply(&self, id: i64, reply: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE items SET reply = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, reply, now()],
        )?;
        self.event(id, "reply", reply)
    }

    pub fn event(&self, id: i64, kind: &str, data: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO item_events (item_id, kind, data, at) VALUES (?1, ?2, ?3, ?4)",
            params![id, kind, data, now()],
        )?;
        Ok(())
    }

    pub fn items_with_status(&self, status: Status) -> Result<Vec<Item>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE status = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([status.as_str()], row_to_item)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Open items first (needs_you, working, planned), then the latest finished ones.
    pub fn recent_items(&self, finished_limit: usize) -> Result<Vec<Item>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE status NOT IN ('done','canceled')
             ORDER BY CASE status WHEN 'needs_you' THEN 0 WHEN 'working' THEN 1 ELSE 2 END, id"
        ))?;
        let mut items: Vec<Item> = stmt.query_map([], row_to_item)?.collect::<Result<_, _>>()?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE status IN ('done','canceled') ORDER BY updated_at DESC LIMIT ?1"
        ))?;
        let limit = i64::try_from(finished_limit).unwrap_or(i64::MAX);
        items.extend(
            stmt.query_map([limit], row_to_item)?
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(items)
    }

    pub fn add_worker(&self, w: &Worker) -> Result<(), StoreError> {
        self.conn.execute(
            &format!("INSERT INTO workers ({WORKER_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
            params![w.name, w.item_id, w.bot, w.repo, w.worktree, w.branch, w.status, w.pr_url, w.summary, w.created_at],
        )?;
        self.event(w.item_id, "worker", &w.name)
    }

    pub fn finish_worker(
        &self,
        name: &str,
        status: &str,
        pr_url: Option<&str>,
        summary: Option<&str>,
    ) -> Result<Option<Worker>, StoreError> {
        self.conn.execute(
            "UPDATE workers SET status = ?2, pr_url = COALESCE(?3, pr_url), summary = COALESCE(?4, summary)
             WHERE name = ?1",
            params![name, status, pr_url, summary],
        )?;
        self.worker(name)
    }

    /// Records what a worker reports without ending it (the daemon ends it when its turn stops).
    pub fn report_worker(
        &self,
        name: &str,
        pr_url: Option<&str>,
        summary: &str,
    ) -> Result<Option<Worker>, StoreError> {
        self.conn.execute(
            "UPDATE workers SET pr_url = COALESCE(?2, pr_url), summary = ?3 WHERE name = ?1",
            params![name, pr_url, summary],
        )?;
        self.worker(name)
    }

    pub fn worker(&self, name: &str) -> Result<Option<Worker>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {WORKER_COLUMNS} FROM workers WHERE name = ?1"),
                [name],
                row_to_worker,
            )
            .optional()?)
    }

    pub fn workers(&self, only_running: bool, limit: usize) -> Result<Vec<Worker>, StoreError> {
        let filter = if only_running {
            "WHERE status = 'running'"
        } else {
            ""
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {WORKER_COLUMNS} FROM workers {filter} ORDER BY created_at DESC LIMIT ?1"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map([limit], row_to_worker)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn running_workers_for(&self, item_id: i64) -> Result<usize, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM workers WHERE item_id = ?1 AND status = 'running'",
            [item_id],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    pub fn workers_for_item(&self, item_id: i64) -> Result<usize, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM workers WHERE item_id = ?1",
            [item_id],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    pub fn paused(&self, bot: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT paused FROM bot_state WHERE bot = ?1", [bot], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .is_some_and(|p| p != 0))
    }

    pub fn set_paused(&self, bot: &str, paused: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO bot_state (bot, paused) VALUES (?1, ?2)
             ON CONFLICT (bot) DO UPDATE SET paused = excluded.paused",
            params![bot, i64::from(paused)],
        )?;
        Ok(())
    }

    /// Marks a trigger key as seen; true when it was new.
    pub fn mark_seen(&self, bot: &str, key: &str) -> Result<bool, StoreError> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO seen (bot, key, at) VALUES (?1, ?2, ?3)",
            params![bot, key, now()],
        )?;
        Ok(n == 1)
    }

    /// Moves everything the store keeps under one bot id to another: items, workers, pause
    /// state, seen trigger keys and trigger baselines. One transaction: all of it or none.
    /// Whatever a deleted bot of the target name left behind is retired first, so the renamed
    /// bot neither collides with it nor inherits it.
    pub fn rename_bot(&self, from: &str, to: &str, tag: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        retire(&tx, to, tag)?;
        for table in ["items", "workers", "bot_state", "seen"] {
            tx.execute(
                &format!("UPDATE {table} SET bot = ?2 WHERE bot = ?1"),
                params![from, to],
            )?;
        }
        // Trigger baselines are keyed "baseline:<bot>:<source>".
        tx.execute(
            "UPDATE cursors SET key = 'baseline:' || ?2 || substr(key, length('baseline:' || ?1) + 1)
             WHERE substr(key, 1, length('baseline:' || ?1 || ':')) = 'baseline:' || ?1 || ':'",
            params![from, to],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Frees a deleted bot's id: its pause state, seen trigger keys and baselines go, and its
    /// history (items, workers) is relabeled `<bot>~<tag>`. A new bot of that name starts clean.
    pub fn retire_bot(&self, bot: &str, tag: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        retire(&tx, bot, tag)?;
        tx.commit()?;
        Ok(())
    }

    pub fn cursor(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM cursors WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_cursor(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO cursors (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat<'a>(bot: &'a str, title: &'a str) -> NewItem<'a> {
        NewItem {
            bot,
            source: "chat",
            external_id: None,
            title,
            body: title,
            url: None,
        }
    }

    #[test]
    fn happy_path() {
        let s = Store::open_in_memory().unwrap();
        let item = s.add_item(&chat("a", "hello")).unwrap().unwrap();
        assert_eq!(item.status, Status::Planned);
        s.transition(item.id, Status::Working, "").unwrap();
        s.set_reply(item.id, "hi").unwrap();
        let done = s.transition(item.id, Status::Done, "").unwrap();
        assert_eq!(done.reply.as_deref(), Some("hi"));
    }

    #[test]
    fn finished_items_stay_finished() {
        let s = Store::open_in_memory().unwrap();
        let id = s.add_item(&chat("a", "x")).unwrap().unwrap().id;
        s.transition(id, Status::Working, "").unwrap();
        s.transition(id, Status::Done, "").unwrap();
        for to in [Status::Planned, Status::Working, Status::NeedsYou] {
            assert!(matches!(
                s.transition(id, to, ""),
                Err(StoreError::BadTransition { .. })
            ));
        }
        assert!(matches!(
            s.transition(99, Status::Done, ""),
            Err(StoreError::NotFound(99))
        ));
    }

    #[test]
    fn needs_you_round_trip_keeps_answer() {
        let s = Store::open_in_memory().unwrap();
        let id = s.add_item(&chat("a", "deploy?")).unwrap().unwrap().id;
        s.transition(id, Status::Working, "").unwrap();
        let asked = s.ask(id, "which env?").unwrap();
        assert_eq!(asked.question.as_deref(), Some("which env?"));
        let answered = s.answer(id, "staging").unwrap();
        assert_eq!(answered.status, Status::Planned);
        assert!(answered.question.is_none());
        assert!(answered.body.ends_with("[answer] staging"));
    }

    #[test]
    fn triggers_dedupe_on_external_id() {
        let s = Store::open_in_memory().unwrap();
        let new = NewItem {
            bot: "a",
            source: "github",
            external_id: Some("o/r#1"),
            title: "t",
            body: "b",
            url: None,
        };
        assert!(s.add_item(&new).unwrap().is_some());
        assert!(s.add_item(&new).unwrap().is_none());
        let other_bot = NewItem {
            bot: "b",
            ..new.clone()
        };
        assert!(s.add_item(&other_bot).unwrap().is_some());
    }

    #[test]
    fn recent_items_puts_needs_you_first() {
        let s = Store::open_in_memory().unwrap();
        let a = s.add_item(&chat("a", "1")).unwrap().unwrap().id;
        let b = s.add_item(&chat("a", "2")).unwrap().unwrap().id;
        s.transition(b, Status::Working, "").unwrap();
        s.ask(b, "?").unwrap();
        let ids: Vec<i64> = s.recent_items(10).unwrap().iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![b, a]);
    }

    #[test]
    fn a_deleted_bots_name_is_free_for_a_rename() {
        let s = Store::open_in_memory().unwrap();
        let trig = |bot: &'static str| NewItem {
            bot,
            source: "github",
            external_id: Some("o/r#1"),
            title: "t",
            body: "b",
            url: None,
        };
        // `foo` saw a trigger, was paused, then was deleted.
        let old = s.add_item(&trig("foo")).unwrap().unwrap().id;
        s.mark_seen("foo", "o/r#1").unwrap();
        s.set_paused("foo", true).unwrap();
        s.set_cursor("baseline:foo:github", "x").unwrap();
        s.retire_bot("foo", "t1").unwrap();
        // `bar` has the same trigger (same unique key) and is renamed to `foo`.
        s.add_item(&trig("bar")).unwrap().unwrap();
        s.rename_bot("bar", "foo", "t2").unwrap();
        assert!(!s.paused("foo").unwrap(), "no inherited pause");
        assert!(
            s.mark_seen("foo", "o/r#1").unwrap(),
            "no inherited seen keys"
        );
        assert!(s.cursor("baseline:foo:github").unwrap().is_none());
        assert_eq!(
            s.item(old).unwrap().bot,
            "foo~t1",
            "old history kept, relabeled"
        );
    }

    #[test]
    fn rename_over_leftovers_from_before_retire_existed() {
        // Rows a deleted bot left behind before deletes retired them: rename still succeeds.
        let s = Store::open_in_memory().unwrap();
        let new = |bot: &'static str| NewItem {
            bot,
            source: "github",
            external_id: Some("k"),
            title: "t",
            body: "b",
            url: None,
        };
        s.add_item(&new("foo")).unwrap();
        s.set_paused("foo", true).unwrap();
        s.add_item(&new("bar")).unwrap();
        s.set_paused("bar", false).unwrap();
        s.rename_bot("bar", "foo", "t").unwrap();
        assert!(!s.paused("foo").unwrap());
    }

    #[test]
    fn workers_and_pause() {
        let s = Store::open_in_memory().unwrap();
        let id = s.add_item(&chat("a", "fix")).unwrap().unwrap().id;
        s.add_worker(&Worker {
            name: "w-1".into(),
            item_id: id,
            bot: "a".into(),
            repo: "/r".into(),
            worktree: "/w".into(),
            branch: "bot/a-1".into(),
            status: "running".into(),
            pr_url: None,
            summary: None,
            created_at: 0,
        })
        .unwrap();
        assert_eq!(s.running_workers_for(id).unwrap(), 1);
        s.finish_worker("w-1", "done", Some("https://x/pull/1"), None)
            .unwrap();
        assert_eq!(s.running_workers_for(id).unwrap(), 0);
        assert!(!s.paused("a").unwrap());
        s.set_paused("a", true).unwrap();
        assert!(s.paused("a").unwrap());
    }
}
