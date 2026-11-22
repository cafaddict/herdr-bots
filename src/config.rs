//! `config.toml` (the daemon) and `bots/<id>/bot.toml` (one bot).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

/// Daemon-wide settings. Every field has a default, so the file is optional.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// The `hc` program (herdr-core).
    pub hc: String,
    pub gh: String,
    /// Seconds between GitHub / Linear polls.
    pub poll_seconds: u64,
    /// Who may trigger bots from GitHub/Linear when a bot doesn't say. Empty = only `gh api user`.
    pub allow_authors: Vec<String>,
    /// Where workers' git worktrees go: `<dir>/<repo>/<branch>`. Same default as Herdr.
    pub worktrees_dir: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            hc: "hc".into(),
            gh: "gh".into(),
            poll_seconds: 120,
            allow_authors: Vec::new(),
            worktrees_dir: None,
        }
    }
}

impl DaemonConfig {
    pub fn load(paths: &Paths) -> Result<Self, ConfigError> {
        let path = paths.config();
        if !path.exists() {
            return Ok(Self::default());
        }
        let config: Self = parse_file(&path)?;
        if config.poll_seconds < 30 {
            return Err(invalid(&path, "poll_seconds must be at least 30"));
        }
        Ok(config)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    #[default]
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// Asks before every tool call.
    Ask,
    /// Edits files freely, asks before commands.
    AcceptEdits,
    /// Never asks.
    #[default]
    FullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleEntry {
    /// 5-field cron (`30 8 * * 1-5`) or `every 30m` (s, m, h, d).
    pub when: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubEvent {
    /// A PR asks for my review.
    ReviewRequested,
    /// An open issue is assigned to me.
    Assigned,
    /// A check failed on one of my open PRs.
    CiFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// `owner/repo`. Empty = every repo the events find.
    #[serde(default)]
    pub repos: Vec<String>,
    pub events: Vec<GithubEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearConfig {
    /// Team key (`ENG`). None = every team.
    pub team: Option<String>,
}

/// One bot, read from `bots/<id>/bot.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    #[serde(skip)]
    pub id: String,
    #[serde(default)]
    pub agent: AgentKind,
    #[serde(default)]
    pub permission: Permission,
    pub model: Option<String>,
    #[serde(default)]
    pub schedule: Vec<ScheduleEntry>,
    pub github: Option<GithubConfig>,
    pub linear: Option<LinearConfig>,
    /// GitHub logins (and Linear emails) whose items this bot acts on. Empty = the daemon default.
    #[serde(default)]
    pub allow_authors: Vec<String>,
}

impl BotConfig {
    pub fn load(path: &Path, id: &str) -> Result<Self, ConfigError> {
        let mut bot: Self = parse_file(path)?;
        bot.id = id.to_owned();
        for entry in &bot.schedule {
            crate::schedule::Schedule::parse(&entry.when)
                .map_err(|e| invalid(path, &format!("schedule `{}`: {e}", entry.when)))?;
        }
        if bot.github.as_ref().is_some_and(|g| g.events.is_empty()) {
            return Err(invalid(path, "[github] needs at least one event"));
        }
        Ok(bot)
    }
}

/// Bot ids become terminal names (`bot-<id>`) and folder names, so keep them to hc's alphabet.
pub fn valid_bot_id(id: &str) -> bool {
    (1..=24).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Every bot folder with a valid `bot.toml`; broken ones come back as errors, keyed by id.
pub fn load_bots(paths: &Paths) -> (BTreeMap<String, BotConfig>, BTreeMap<String, String>) {
    let mut bots = BTreeMap::new();
    let mut errors = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(paths.bots_dir()) else {
        return (bots, errors);
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        let file = entry.path().join("bot.toml");
        if !valid_bot_id(&id) || !file.is_file() {
            continue;
        }
        match BotConfig::load(&file, &id) {
            Ok(bot) => {
                bots.insert(id, bot);
            }
            Err(e) => {
                errors.insert(id, e.to_string());
            }
        }
    }
    (bots, errors)
}

fn parse_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.display().to_string(),
        source,
    })
}

fn invalid(path: &Path, message: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.display().to_string(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, id: &str, text: &str) {
        let home = dir.join("bots").join(id);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("bot.toml"), text).unwrap();
    }

    #[test]
    fn minimal_bot_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "scout", "");
        let (bots, errors) = load_bots(&Paths::new(tmp.path()));
        assert!(errors.is_empty(), "{errors:?}");
        let bot = &bots["scout"];
        assert_eq!(bot.agent, AgentKind::Claude);
        assert_eq!(bot.permission, Permission::FullAccess);
    }

    #[test]
    fn full_bot_parses() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "rev",
            r#"
agent = "codex"
permission = "accept-edits"
model = "gpt-5"
allow_authors = ["me"]
[[schedule]]
when = "30 8 * * 1-5"
prompt = "morning"
[github]
repos = ["o/r"]
events = ["review-requested", "ci-failed"]
[linear]
team = "ENG"
"#,
        );
        let (bots, errors) = load_bots(&Paths::new(tmp.path()));
        assert!(errors.is_empty(), "{errors:?}");
        let bot = &bots["rev"];
        assert_eq!(bot.agent, AgentKind::Codex);
        assert_eq!(bot.github.as_ref().unwrap().events.len(), 2);
    }

    #[test]
    fn bad_schedule_and_unknown_key_are_errors_not_panics() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "a",
            "[[schedule]]\nwhen = \"every\"\nprompt = \"x\"\n",
        );
        write(tmp.path(), "b", "colour = \"red\"\n");
        write(tmp.path(), "c", "[github]\nevents = []\n");
        let (bots, errors) = load_bots(&Paths::new(tmp.path()));
        assert!(bots.is_empty());
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn bot_ids() {
        assert!(valid_bot_id("scout-2"));
        assert!(!valid_bot_id("Scout"));
        assert!(!valid_bot_id("a/b"));
        assert!(!valid_bot_id(""));
    }
}
