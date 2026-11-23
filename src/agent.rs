//! Starting an agent (Claude Code / Codex) in an hc terminal and driving one turn of it.

use std::path::Path;

use crate::config::{AgentKind, Permission};
use crate::hc::{AgentStatus, Hc, HcError, WaitOutcome};

/// How long one `hc wait` blocks before it checks in. Waits loop, so this is not a turn limit.
const WAIT_SECS: u64 = 300;
/// Readiness check-ins before giving up on a freshly started agent.
const READY_CHECKINS: u32 = 18; // x 10 s
/// Pause after pressing the trust keys before looking at the screen again.
const TRUST_REDRAW: std::time::Duration = std::time::Duration::from_secs(1);

/// The command line for an interactive agent session (never a one-shot print mode: hc's rule,
/// so a person can attach and take over).
pub fn agent_argv(agent: AgentKind, permission: Permission, model: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    match agent {
        AgentKind::Claude => {
            argv.push("claude".into());
            match permission {
                Permission::FullAccess => argv.push("--dangerously-skip-permissions".into()),
                Permission::AcceptEdits => {
                    argv.extend(["--permission-mode".into(), "acceptEdits".into()])
                }
                Permission::Ask => {}
            }
            if let Some(m) = model {
                argv.extend(["--model".into(), m.into()]);
            }
        }
        AgentKind::Codex => {
            argv.push("codex".into());
            match permission {
                Permission::FullAccess => {
                    argv.push("--dangerously-bypass-approvals-and-sandbox".into())
                }
                Permission::AcceptEdits => argv.push("--full-auto".into()),
                Permission::Ask => argv.extend(["--ask-for-approval".into(), "untrusted".into()]),
            }
            if let Some(m) = model {
                argv.extend(["-m".into(), m.into()]);
            }
        }
    }
    argv
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// Waiting on a person: a trust or permission dialog on first start.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    Finished,
    Blocked,
    Exited(Option<i32>),
    Failed,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Hc(#[from] HcError),
    #[error("{0} did not become ready")]
    NotReady(String),
    #[error("{0} exited while starting (code {1:?})")]
    ExitedOnStart(String, Option<i32>),
}

/// Makes sure a live, ready agent terminal called `name` exists, starting one if needed.
pub async fn ensure_agent(
    hc: &Hc,
    name: &str,
    cwd: &Path,
    labels: &[(&str, &str)],
    argv: &[String],
) -> Result<Readiness, AgentError> {
    match hc.get(name).await? {
        // No status yet means it's still starting, maybe on the trust dialog: settle below.
        Some(t) if t.is_live() => {
            if let Some(ready) = live_step(t.status) {
                return Ok(ready);
            }
        }
        Some(_) => {
            hc.rm(name).await?; // a finished one keeps the name until removed
            hc.run_bg(name, cwd, labels, argv).await?;
        }
        None => {
            hc.run_bg(name, cwd, labels, argv).await?;
        }
    }
    settle(hc, name).await
}

/// What a terminal that's already running needs. `None`: it hasn't reported a status yet, so
/// it's still starting and `settle` may answer its folder-trust dialog.
///
/// A session that has reported `blocked` is past startup: it waits on a person (a tool
/// permission prompt, a question). Never press keys into that, whatever its screen shows.
fn live_step(status: Option<AgentStatus>) -> Option<Readiness> {
    match status {
        None => None,
        Some(AgentStatus::Blocked) => Some(Readiness::Blocked),
        Some(_) => Some(Readiness::Ready),
    }
}

/// Waits for a starting agent to report a status, getting it past the folder-trust dialog.
///
/// herdr-bots made the folders its agents start in (a bot's home, a worker's worktree), so
/// trusting them is ours to answer. Anything else on first start (Claude's one-time consent to
/// bypass permissions) is the person's call: that comes back as `Blocked`.
async fn settle(hc: &Hc, name: &str) -> Result<Readiness, AgentError> {
    for _ in 0..READY_CHECKINS {
        let w = hc.wait(name, 10).await?;
        if w.reason.as_deref() == Some("exit") {
            return Err(AgentError::ExitedOnStart(name.into(), w.exit_code));
        }
        let screen = if is_ready(w.status) {
            String::new()
        } else {
            hc.read(name, 0).await.unwrap_or_default()
        };
        match start_step(w.status, &screen) {
            StartStep::Ready => return Ok(Readiness::Ready),
            StartStep::Blocked => return Ok(Readiness::Blocked),
            // Claude drops keys that arrive before it takes input, which hc's `blocked` can
            // precede, so this presses again while the dialog stays. The keys follow the
            // cursor, so a press that only moved it is finished, not undone.
            StartStep::Trust(keys) => {
                hc.keys(name, keys).await?;
                tokio::time::sleep(TRUST_REDRAW).await;
            }
            StartStep::Wait => {}
        }
    }
    Err(AgentError::NotReady(name.into()))
}

fn is_ready(status: Option<AgentStatus>) -> bool {
    matches!(status, Some(AgentStatus::Idle | AgentStatus::Done))
}

#[derive(Debug, PartialEq, Eq)]
enum StartStep {
    Ready,
    Blocked,
    Trust(&'static [&'static str]),
    Wait,
}

/// hc reports the folder-trust dialog as `blocked` too, so the screen decides before the status.
fn start_step(status: Option<AgentStatus>, screen: &str) -> StartStep {
    if is_ready(status) {
        return StartStep::Ready;
    }
    match first_start_dialog(screen) {
        Some(Dialog::FolderTrust { keys }) => StartStep::Trust(keys),
        Some(Dialog::NeedsPerson) => StartStep::Blocked,
        None if status == Some(AgentStatus::Blocked) => StartStep::Blocked,
        None => StartStep::Wait,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Dialog {
    /// The keys that pick "trust" on it.
    FolderTrust {
        keys: &'static [&'static str],
    },
    NeedsPerson,
}

/// How many of the screen's last non-empty lines a first-start dialog fills. Text above them is
/// whatever scrolled by (a diff, a file) and must not count as the dialog.
const DIALOG_TAIL: usize = 16;

fn first_start_dialog(screen: &str) -> Option<Dialog> {
    let tail: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = tail[tail.len().saturating_sub(DIALOG_TAIL)..].join("\n");
    let screen = tail.as_str();
    if screen.contains("Bypass Permissions mode") || screen.contains("Yes, I accept") {
        return Some(Dialog::NeedsPerson);
    }
    // Claude Code: "Accessing workspace: ... ❯ No, exit / Yes, I trust this folder".
    if screen.contains("Accessing workspace") && screen.contains("Yes, I trust this folder") {
        let cursor_on = |option: &str| {
            screen
                .lines()
                .any(|l| l.contains('❯') && l.contains(option))
        };
        let no_first = screen
            .find("No, exit")
            .is_some_and(|no| Some(no) < screen.find("Yes, I trust this folder"));
        let on_no = cursor_on("No, exit") || (no_first && !cursor_on("Yes, I trust this folder"));
        return Some(Dialog::FolderTrust {
            keys: if on_no {
                &["down", "enter"]
            } else {
                &["enter"]
            },
        });
    }
    if screen.contains("Do you trust the files in this folder?") && screen.contains("Yes, proceed")
    {
        return Some(Dialog::FolderTrust { keys: &["enter"] });
    }
    None
}

/// Sends `text` and waits until the turn it starts is over, however long that takes.
pub async fn drive_turn(hc: &Hc, name: &str, text: &str) -> Result<TurnEnd, HcError> {
    let mut outcome = hc.send_wait(name, text, WAIT_SECS).await?;
    loop {
        if let Some(end) = classify(&outcome) {
            return Ok(end);
        }
        outcome = hc.wait(name, WAIT_SECS).await?;
    }
}

/// `None` = still going, wait again.
fn classify(w: &WaitOutcome) -> Option<TurnEnd> {
    if w.reason.as_deref() == Some("exit") {
        return Some(TurnEnd::Exited(w.exit_code));
    }
    if w.is_checkin() || w.reason.as_deref() == Some("not_started") {
        return None;
    }
    match w.status {
        Some(AgentStatus::Working) => None,
        Some(AgentStatus::Blocked) => Some(TurnEnd::Blocked),
        Some(AgentStatus::Error) => Some(TurnEnd::Failed),
        _ => Some(TurnEnd::Finished),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialogs() {
        let trust =
            "Accessing workspace:\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm";
        assert_eq!(
            first_start_dialog(trust),
            Some(Dialog::FolderTrust {
                keys: &["down", "enter"]
            })
        );
        let bypass = "WARNING: Claude Code running in Bypass Permissions mode\n ❯ No, exit\n   Yes, I accept";
        assert_eq!(first_start_dialog(bypass), Some(Dialog::NeedsPerson));
        assert_eq!(first_start_dialog("> try \"fix lint errors\""), None);
    }

    #[test]
    fn a_blocked_folder_trust_dialog_is_answered() {
        let trust = " Accessing workspace:\n\n /home/me/.herdr/worktrees/repo/bot/cos-9-1\n\n \
                     ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel";
        assert_eq!(
            start_step(Some(AgentStatus::Blocked), trust),
            StartStep::Trust(&["down", "enter"])
        );
        assert_eq!(
            start_step(None, trust),
            StartStep::Trust(&["down", "enter"])
        );
    }

    #[test]
    fn trust_keys_follow_the_cursor() {
        let moved = " Accessing workspace:\n ❯ Yes, I trust this folder\n   No, exit";
        assert_eq!(
            start_step(Some(AgentStatus::Blocked), moved),
            StartStep::Trust(&["enter"])
        );
        let half_pressed =
            " Accessing workspace:\n   No, exit\n ❯ Yes, I trust this folder\n Enter to confirm";
        assert_eq!(
            start_step(Some(AgentStatus::Blocked), half_pressed),
            StartStep::Trust(&["enter"])
        );
    }

    #[test]
    fn other_start_states() {
        let bypass = "WARNING: Claude Code running in Bypass Permissions mode\n ❯ No, exit\n   Yes, I accept";
        assert_eq!(
            start_step(Some(AgentStatus::Blocked), bypass),
            StartStep::Blocked
        );
        assert_eq!(
            start_step(Some(AgentStatus::Blocked), "Allow this tool?"),
            StartStep::Blocked
        );
        assert_eq!(start_step(Some(AgentStatus::Idle), ""), StartStep::Ready);
        assert_eq!(start_step(Some(AgentStatus::Done), ""), StartStep::Ready);
        assert_eq!(start_step(None, "starting"), StartStep::Wait);
        assert_eq!(
            start_step(Some(AgentStatus::Working), "starting"),
            StartStep::Wait
        );
    }

    #[test]
    fn a_running_blocked_session_is_never_answered() {
        // A tool-permission prompt in a session that already reported a status: whatever its
        // screen shows, it goes to the person.
        assert_eq!(
            live_step(Some(AgentStatus::Blocked)),
            Some(Readiness::Blocked)
        );
        assert_eq!(live_step(Some(AgentStatus::Idle)), Some(Readiness::Ready));
        assert_eq!(
            live_step(Some(AgentStatus::Working)),
            Some(Readiness::Ready)
        );
        assert_eq!(live_step(None), None);
    }

    #[test]
    fn trust_text_in_scrolled_output_is_not_the_dialog() {
        // This repo's own test strings in a diff above a permission prompt.
        let mut screen = String::from(
            " Accessing workspace:\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm\n",
        );
        for n in 0..20 {
            screen.push_str(&format!("  {n} +    let other = line_{n}();\n"));
        }
        screen.push_str(
            " Do you want to make this edit to agent.rs?\n ❯ 1. Yes\n   2. Yes, and don't ask again\n   3. No\n",
        );
        assert_eq!(first_start_dialog(&screen), None);
        assert_eq!(
            start_step(Some(AgentStatus::Blocked), &screen),
            StartStep::Blocked
        );
        // Without the dialog's header the options alone don't count either.
        assert_eq!(
            first_start_dialog(" ❯ No, exit\n   Yes, I trust this folder"),
            None
        );
    }

    #[test]
    fn argv_per_permission() {
        let full = agent_argv(AgentKind::Claude, Permission::FullAccess, Some("opus"));
        assert_eq!(
            full,
            [
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                "opus"
            ]
        );
        assert_eq!(
            agent_argv(AgentKind::Claude, Permission::Ask, None),
            ["claude"]
        );
        let codex = agent_argv(AgentKind::Codex, Permission::AcceptEdits, None);
        assert_eq!(codex, ["codex", "--full-auto"]);
    }

    fn outcome(reason: Option<&str>, status: Option<AgentStatus>) -> WaitOutcome {
        WaitOutcome {
            ended: false,
            reason: reason.map(Into::into),
            status,
            exit_code: None,
        }
    }

    #[test]
    fn classify_waits_through_checkins() {
        assert_eq!(
            classify(&outcome(Some("timeout"), Some(AgentStatus::Working))),
            None
        );
        assert_eq!(classify(&outcome(Some("not_started"), None)), None);
        assert_eq!(
            classify(&outcome(Some("done"), Some(AgentStatus::Done))),
            Some(TurnEnd::Finished)
        );
        assert_eq!(
            classify(&outcome(Some("blocked"), Some(AgentStatus::Blocked))),
            Some(TurnEnd::Blocked)
        );
        assert_eq!(
            classify(&outcome(Some("exit"), None)),
            Some(TurnEnd::Exited(None))
        );
    }
}
