//! Where herdr-bots keeps its files.
//!
//! Everything lives under one root (`$HERDR_BOTS_HOME`, default `~/.herdr-bots`) so tests can
//! point the whole program at a temporary folder by setting one variable.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$HERDR_BOTS_HOME`, else `~/.herdr-bots`.
    pub fn from_env() -> anyhow::Result<Self> {
        if let Some(root) = std::env::var_os("HERDR_BOTS_HOME") {
            return Ok(Self::new(root));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        Ok(Self::new(Path::new(&home).join(".herdr-bots")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn bots_dir(&self) -> PathBuf {
        self.root.join("bots")
    }
    pub fn bot_home(&self, bot: &str) -> PathBuf {
        self.bots_dir().join(bot)
    }
    pub fn outbox(&self, bot: &str, item: i64) -> PathBuf {
        self.bot_home(bot).join("outbox").join(format!("{item}.md"))
    }
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn db(&self) -> PathBuf {
        self.state_dir().join("state.db")
    }
    /// Snapshot the herdr mod reads (it watches `state_dir`).
    pub fn status_json(&self) -> PathBuf {
        self.state_dir().join("status.json")
    }
    /// `$HERDR_BOTS_SOCKET`, else `<root>/botsd.sock`. Unix socket paths are limited to ~100
    /// bytes, so a deep root needs the override.
    pub fn socket(&self) -> PathBuf {
        match std::env::var_os("HERDR_BOTS_SOCKET") {
            Some(p) => PathBuf::from(p),
            None => self.root.join("botsd.sock"),
        }
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [self.bots_dir(), self.state_dir(), self.logs_dir()] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
