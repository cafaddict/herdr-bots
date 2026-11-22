//! Outside events that file items: GitHub and Linear, both polled.
//!
//! The first poll for a bot only records what already exists (the baseline), so turning a
//! trigger on doesn't flood the bot with every open issue. After that, anything new is filed.

pub mod github;
pub mod linear;

use std::sync::Arc;

use tracing::warn;

use crate::app::{App, Turn};
use crate::config::BotConfig;
use crate::store::NewItem;

/// Something a trigger found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Stable dedupe key, e.g. `review:o/r#12`.
    pub key: String,
    pub source: &'static str,
    pub title: String,
    pub body: String,
    pub url: String,
    /// GitHub login or Linear email of whoever made it, checked against the allow list.
    pub author: String,
}

/// Case-insensitive; an empty list allows nobody.
pub fn allowed(author: &str, allow: &[String]) -> bool {
    allow.iter().any(|a| a.eq_ignore_ascii_case(author))
}

/// Files what's new for one bot and source. Returns how many items were filed.
pub fn file_found(
    app: &App,
    bot: &BotConfig,
    source: &str,
    found: Vec<Found>,
    allow: &[String],
) -> usize {
    let baseline_key = format!("baseline:{}:{source}", bot.id);
    let store = app.store();
    let first_poll = store.cursor(&baseline_key).ok().flatten().is_none();
    let mut filed = Vec::new();
    for f in found {
        let new = match store.mark_seen(&bot.id, &f.key) {
            Ok(new) => new,
            Err(e) => {
                warn!(bot = bot.id, "seen: {e}");
                continue;
            }
        };
        if !new || first_poll || !allowed(&f.author, allow) {
            continue;
        }
        let item = NewItem {
            bot: &bot.id,
            source: f.source,
            external_id: Some(&f.key),
            title: &f.title,
            body: &f.body,
            url: Some(&f.url),
        };
        match store.add_item(&item) {
            Ok(Some(item)) => filed.push(item.id),
            Ok(None) => {}
            Err(e) => warn!(bot = bot.id, "file item: {e}"),
        }
    }
    if first_poll {
        let _ = store.set_cursor(&baseline_key, &chrono::Utc::now().to_rfc3339());
    }
    drop(store);
    let n = filed.len();
    for id in filed {
        app.enqueue(
            &bot.id,
            Turn {
                item: id,
                note: None,
            },
        );
    }
    n
}

/// Polls every trigger of every bot, forever.
pub async fn poll_loop(app: Arc<App>) {
    let period = std::time::Duration::from_secs(app.config.poll_seconds);
    loop {
        if app.me.read().map(|m| m.is_none()).unwrap_or(true)
            && let Ok(login) = github::whoami(&app.config.gh).await
            && let Ok(mut me) = app.me.write()
        {
            *me = Some(login);
        }
        for bot in app.bots() {
            let allow = app.allow_list(&bot);
            if let Some(cfg) = &bot.github {
                match github::poll(&app.config.gh, cfg).await {
                    Ok(found) => {
                        file_found(&app, &bot, "github", found, &allow);
                    }
                    Err(e) => warn!(bot = bot.id, "github poll: {e:#}"),
                }
            }
            if let Some(cfg) = &bot.linear {
                match linear::poll(&app.paths, cfg).await {
                    Ok(found) => {
                        // Linear workspaces are private: when the allow list names no emails,
                        // trust any creator in the workspace.
                        let emails: Vec<String> =
                            allow.iter().filter(|a| a.contains('@')).cloned().collect();
                        let allow_all: Vec<String> =
                            found.iter().map(|f| f.author.clone()).collect();
                        file_found(
                            &app,
                            &bot,
                            "linear",
                            found,
                            if emails.is_empty() {
                                &allow_all
                            } else {
                                &emails
                            },
                        );
                    }
                    Err(e) => warn!(bot = bot.id, "linear poll: {e:#}"),
                }
            }
        }
        tokio::time::sleep(period).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DaemonConfig, Permission};
    use crate::paths::Paths;
    use crate::store::Store;

    fn found(key: &str, author: &str) -> Found {
        Found {
            key: key.into(),
            source: "github",
            title: "t".into(),
            body: "b".into(),
            url: "u".into(),
            author: author.into(),
        }
    }

    fn bot() -> BotConfig {
        BotConfig {
            id: "a".into(),
            agent: Default::default(),
            permission: Permission::Ask,
            model: None,
            schedule: vec![],
            github: None,
            linear: None,
            allow_authors: vec![],
        }
    }

    #[test]
    fn baseline_then_only_new_and_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let app = App::new(
            Paths::new(tmp.path()),
            DaemonConfig::default(),
            Store::open_in_memory().unwrap(),
            "x".into(),
        );
        let allow = vec!["Me".to_string()];
        // First poll: baseline only.
        assert_eq!(
            file_found(&app, &bot(), "github", vec![found("k1", "me")], &allow),
            0
        );
        // k1 is old, k2 is from a stranger, k3 is new and mine.
        let n = file_found(
            &app,
            &bot(),
            "github",
            vec![
                found("k1", "me"),
                found("k2", "stranger"),
                found("k3", "ME"),
            ],
            &allow,
        );
        assert_eq!(n, 1);
        // A stranger's item stays ignored even if they're added later (seen once = done).
        assert_eq!(
            file_found(
                &app,
                &bot(),
                "github",
                vec![found("k2", "stranger")],
                &["stranger".into()]
            ),
            0
        );
    }

    #[test]
    fn empty_allow_list_allows_nobody() {
        assert!(!allowed("me", &[]));
    }
}
