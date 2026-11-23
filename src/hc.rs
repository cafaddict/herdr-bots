//! A thin async wrapper around herdr-core's `hc` CLI.
//!
//! We shell out instead of speaking hc's socket protocol: the CLI is hc's stable, documented
//! surface (`hc --skill`), and it means `hc` can be swapped for a fake script in tests.

use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum HcError {
    #[error("could not start `{program}`: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("hc {command}: {kind}: {message}")]
    Failed {
        command: String,
        kind: String,
        message: String,
    },
    #[error("hc {command}: unexpected output: {output}")]
    BadOutput { command: String, output: String },
}

impl HcError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, HcError::Failed { kind, .. } if kind == "not_found")
    }
}

/// An agent's status as hc reports it. `None` for plain programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Working,
    Idle,
    Done,
    Blocked,
    Error,
    #[serde(other)]
    Unknown,
}

/// hc reports an agent's status as a word (`"idle"`) in `ls`, `wait` and `send`, but `get`
/// (hc 0.1.21) gives an object such as `{"app": "claude-code", "state": "idle"}`. Takes either.
fn status_word_or_object<'de, D: Deserializer<'de>>(d: D) -> Result<Option<AgentStatus>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Word(AgentStatus),
        Detailed {
            #[serde(default)]
            state: Option<AgentStatus>,
        },
    }
    Ok(match Option::<Repr>::deserialize(d)? {
        Some(Repr::Word(s)) => Some(s),
        Some(Repr::Detailed { state }) => state,
        None => None,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Terminal {
    pub address: String,
    pub name: Option<String>,
    pub state: String,
    #[serde(default, deserialize_with = "status_word_or_object")]
    pub status: Option<AgentStatus>,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    pub exit_code: Option<i32>,
}

impl Terminal {
    pub fn is_live(&self) -> bool {
        self.state == "live"
    }
}

/// Why a wait came back.
#[derive(Debug, Clone, Deserialize)]
pub struct WaitOutcome {
    #[serde(default)]
    pub ended: bool,
    /// `exit`, `timeout`, `message`, `done`, `blocked`, `idle`, `error`, `not_started`, ...
    pub reason: Option<String>,
    #[serde(default, deserialize_with = "status_word_or_object")]
    pub status: Option<AgentStatus>,
    pub exit_code: Option<i32>,
}

impl WaitOutcome {
    /// A timeout is only a check-in (the hc manual says to wait again).
    pub fn is_checkin(&self) -> bool {
        matches!(self.reason.as_deref(), Some("timeout" | "message"))
    }
}

#[derive(Debug, Clone)]
pub struct Hc {
    program: String,
}

impl Hc {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    async fn json(&self, args: &[&str]) -> Result<Value, HcError> {
        let command = args.first().copied().unwrap_or_default().to_owned();
        // `--json` right after the subcommand, so it can't be mistaken for part of a `--` tail.
        let output = Command::new(&self.program)
            .args(args.first())
            .arg("--json")
            .args(args.get(1..).unwrap_or_default())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| HcError::Spawn {
                program: self.program.clone(),
                source,
            })?;
        // Results come on stdout; errors (`{"error": ...}`) on stderr with a nonzero exit.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let text = if stdout.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        let value: Value = serde_json::from_str(text).map_err(|_| HcError::BadOutput {
            command: command.clone(),
            output: format!("{}{}", stdout.trim(), stderr.trim()),
        })?;
        if let Some(err) = value.get("error") {
            let field = |k: &str| {
                err.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            return Err(HcError::Failed {
                command,
                kind: field("kind"),
                message: field("message"),
            });
        }
        Ok(value)
    }

    fn parse<T: serde::de::DeserializeOwned>(command: &str, value: Value) -> Result<T, HcError> {
        let text = value.to_string();
        serde_json::from_value(value).map_err(|_| HcError::BadOutput {
            command: command.into(),
            output: text,
        })
    }

    /// Starts `argv` in a new background terminal that belongs to nobody, so it outlives us.
    pub async fn run_bg(
        &self,
        name: &str,
        cwd: &Path,
        labels: &[(&str, &str)],
        argv: &[String],
    ) -> Result<String, HcError> {
        let cwd = cwd.to_string_lossy();
        let labels: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut args = vec![
            "run",
            "--bg",
            "--name",
            name,
            "--cwd",
            &cwd,
            "--no-parent",
            "--no-notify",
        ];
        for label in &labels {
            args.extend(["--label", label]);
        }
        args.push("--");
        args.extend(argv.iter().map(String::as_str));
        let value = self.json(&args).await?;
        value
            .get("address")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| HcError::BadOutput {
                command: "run".into(),
                output: value.to_string(),
            })
    }

    /// `None` when no such terminal exists.
    pub async fn get(&self, terminal: &str) -> Result<Option<Terminal>, HcError> {
        match self.json(&["get", terminal]).await {
            Ok(v) => Self::parse("get", v).map(Some),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn wait(&self, terminal: &str, timeout_secs: u64) -> Result<WaitOutcome, HcError> {
        let timeout = timeout_secs.to_string();
        let v = self
            .json(&["wait", terminal, "--timeout", &timeout])
            .await?;
        Self::parse("wait", v)
    }

    /// Pastes `text`, presses Enter, and waits for the turn it starts.
    pub async fn send_wait(
        &self,
        terminal: &str,
        text: &str,
        timeout_secs: u64,
    ) -> Result<WaitOutcome, HcError> {
        let timeout = timeout_secs.to_string();
        let v = self
            .json(&[
                "send",
                "--wait",
                "--timeout",
                &timeout,
                "--",
                terminal,
                text,
            ])
            .await?;
        match v.get("wait") {
            Some(wait) => Self::parse("send", wait.clone()),
            // Sent but the turn hadn't started yet.
            None => Ok(WaitOutcome {
                ended: false,
                reason: Some("not_started".into()),
                status: status_word_or_object(v.get("status").cloned().unwrap_or(Value::Null))
                    .ok()
                    .flatten(),
                exit_code: None,
            }),
        }
    }

    pub async fn read(&self, terminal: &str, lines: u32) -> Result<String, HcError> {
        let lines = lines.to_string();
        let output = Command::new(&self.program)
            .args(["read", terminal, "--lines", &lines])
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|source| HcError::Spawn {
                program: self.program.clone(),
                source,
            })?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Presses keys (`down`, `enter`, `esc`, `ctrl+c`, ...).
    pub async fn keys(&self, terminal: &str, keys: &[&str]) -> Result<(), HcError> {
        let mut args = vec!["keys", terminal];
        args.extend_from_slice(keys);
        self.json(&args).await.map(drop)
    }

    pub async fn interrupt(&self, terminal: &str) -> Result<(), HcError> {
        self.json(&["interrupt", terminal]).await.map(drop)
    }

    pub async fn kill(&self, terminal: &str) -> Result<(), HcError> {
        self.json(&["kill", terminal]).await.map(drop)
    }

    /// Removes a finished terminal so its name can be reused. Missing is fine.
    pub async fn rm(&self, terminal: &str) -> Result<(), HcError> {
        match self.json(&["rm", terminal]).await {
            Err(e) if !e.is_not_found() => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod status_shape_tests {
    use super::*;

    fn terminal(status: &str) -> Terminal {
        serde_json::from_str(&format!(
            r#"{{"address":"t12","name":"bot-x","state":"live","status":{status},"labels":{{}},"exit_code":null}}"#
        ))
        .unwrap()
    }

    #[test]
    fn get_status_as_an_object_or_a_word() {
        assert_eq!(
            terminal(r#"{"app":"claude-code","state":"idle"}"#).status,
            Some(AgentStatus::Idle)
        );
        assert_eq!(terminal(r#""blocked""#).status, Some(AgentStatus::Blocked));
        assert_eq!(terminal("null").status, None);
        assert_eq!(terminal(r#"{"app":"bash"}"#).status, None);
    }

    #[test]
    fn wait_status_still_reads_as_a_word() {
        let w: WaitOutcome = serde_json::from_str(
            r#"{"ended":true,"reason":"status","status":"idle","exit_code":null}"#,
        )
        .unwrap();
        assert_eq!(w.status, Some(AgentStatus::Idle));
    }
}
