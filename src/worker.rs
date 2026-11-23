//! Workers: an agent in its own git worktree that does one item's code change and reports back.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use tokio::process::Command;
use tracing::{info, warn};

use crate::agent::{Readiness, TurnEnd, agent_argv, drive_turn, ensure_agent};
use crate::app::{App, Turn};
use crate::store::{Status, Worker};

pub struct StartRequest {
    pub item: i64,
    pub repo: PathBuf,
    pub spec: String,
    pub base: Option<String>,
}

/// Makes the worktree and records the worker, then starts its agent in the background.
/// Returns as soon as the worktree exists, so the calling bot's turn isn't held up.
pub async fn start(app: &Arc<App>, req: StartRequest) -> anyhow::Result<Worker> {
    let item = app.store().item(req.item)?;
    if item.status.is_final() {
        bail!("item {} is {}", item.id, item.status.as_str());
    }
    let bot = app
        .bot(&item.bot)
        .with_context(|| format!("bot {} is not loaded", item.bot))?;

    let top = git(&req.repo, &["rev-parse", "--show-toplevel"])
        .await
        .context("not a git repository")?;
    let top = PathBuf::from(top.trim());
    let repo_name = top
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_owned();
    let n = app.store().workers_for_item(item.id)? + 1;
    let name = format!("w{}-{n}", item.id);
    let branch = format!("bot/{}-{}-{n}", item.bot, item.id);
    let worktree = worktrees_dir(app).join(&repo_name).join(&branch);
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let worktree_str = worktree.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add", "-b", &branch, &worktree_str];
    if let Some(base) = &req.base {
        args.push(base);
    }
    git(&top, &args).await.context("git worktree add")?;

    let worker = Worker {
        name: name.clone(),
        item_id: item.id,
        bot: item.bot.clone(),
        repo: top.to_string_lossy().into_owned(),
        worktree: worktree_str,
        branch: branch.clone(),
        status: "running".into(),
        pr_url: None,
        summary: None,
        created_at: chrono::Utc::now().timestamp_millis(),
    };
    app.store().add_worker(&worker)?;
    app.changed.notify_one();
    info!(worker = name, item = item.id, "worker started");

    let argv = agent_argv(bot.agent, bot.permission, bot.model.as_deref());
    let spec = worker_message(&req.spec, &branch, &name, &app.exe);
    let app = Arc::clone(app);
    let w = worker.clone();
    tokio::spawn(async move {
        let end = run(&app, &w, &argv, &spec).await;
        report_back(&app, &w, end).await;
    });
    Ok(worker)
}

async fn run(app: &App, w: &Worker, argv: &[String], spec: &str) -> anyhow::Result<TurnEnd> {
    let item = w.item_id.to_string();
    let labels = [
        ("herdr-worker", item.as_str()),
        ("herdr-bot", w.bot.as_str()),
    ];
    match ensure_agent(&app.hc, &w.name, Path::new(&w.worktree), &labels, argv).await? {
        Readiness::Blocked => Ok(TurnEnd::Blocked),
        Readiness::Ready => Ok(drive_turn(&app.hc, &w.name, spec).await?),
    }
}

/// Re-attaches to workers that were running when the daemon stopped.
pub fn resume_running(app: &Arc<App>) {
    let running = app.store().workers(true, 1000).unwrap_or_default();
    for w in running {
        let app = Arc::clone(app);
        tokio::spawn(async move {
            let end = async {
                loop {
                    let outcome = app.hc.wait(&w.name, 300).await?;
                    if outcome.is_checkin()
                        || outcome.status == Some(crate::hc::AgentStatus::Working)
                    {
                        continue;
                    }
                    return Ok(match (outcome.reason.as_deref(), outcome.status) {
                        (Some("exit"), _) => TurnEnd::Exited(outcome.exit_code),
                        (_, Some(crate::hc::AgentStatus::Blocked)) => TurnEnd::Blocked,
                        _ => TurnEnd::Finished,
                    });
                }
            }
            .await;
            report_back(&app, &w, end).await;
        });
    }
}

async fn report_back(app: &App, w: &Worker, end: anyhow::Result<TurnEnd>) {
    let (status, note) = match &end {
        Ok(TurnEnd::Finished) => ("done", "finished"),
        Ok(TurnEnd::Blocked) => (
            "failed",
            "is blocked on a permission dialog in its terminal",
        ),
        Ok(TurnEnd::Exited(_)) => ("failed", "exited"),
        Ok(TurnEnd::Failed) => ("failed", "reported an error"),
        Err(e) => {
            warn!(worker = w.name, "worker failed: {e:#}");
            ("failed", "could not run")
        }
    };
    if status == "failed" && end.is_ok() {
        warn!(worker = w.name, "worker {note}");
    }
    // A blocked worker keeps its terminal: the owner may answer the dialog there.
    let cleanup = match &end {
        Ok(TurnEnd::Blocked) => None,
        _ => Some(clean_up(app, w).await),
    };
    let finished = {
        let store = app.store();
        let done = store
            .finish_worker(&w.name, status, None, None)
            .ok()
            .flatten();
        if status == "failed" && done.as_ref().is_some_and(|d| d.summary.is_none()) {
            let reason = match &end {
                Err(e) => format!("{note}: {e:#}"),
                Ok(_) => note.to_owned(),
            };
            let _ = store.report_worker(&w.name, None, &reason);
        }
        let still_open = store
            .item(w.item_id)
            .is_ok_and(|i| i.status == Status::Working);
        done.filter(|_| still_open)
    };
    app.changed.notify_one();
    let Some(done) = finished else { return };
    let mut text = format!(
        "worker {} ({}, branch {}) {note}.",
        done.name, done.worktree, done.branch
    );
    if let Some(cleanup) = &cleanup {
        text.push_str(&format!(" {}.", cleanup.note()));
    }
    if let Some(pr) = &done.pr_url {
        text.push_str(&format!(" PR: {pr}."));
    }
    if let Some(summary) = &done.summary {
        text.push_str(&format!(" Its report: {summary}"));
    }
    if let Err(e) = &end {
        text.push_str(&format!(" Error: {e:#}"));
    }
    app.enqueue(
        &w.bot,
        Turn {
            item: w.item_id,
            note: Some(text),
        },
    );
}

/// Ends a finished worker's terminal, then removes its worktree if it is clean.
async fn clean_up(app: &App, w: &Worker) -> WorktreeCleanup {
    let _ = app.hc.kill(&w.name).await;
    let _ = app.hc.rm(&w.name).await;
    let cleanup = remove_worktree_if_clean(Path::new(&w.repo), Path::new(&w.worktree)).await;
    match &cleanup {
        WorktreeCleanup::Removed => info!(worker = w.name, "worktree removed"),
        WorktreeCleanup::KeptDirty => warn!(
            worker = w.name,
            worktree = w.worktree,
            "worktree kept: it has uncommitted changes"
        ),
        WorktreeCleanup::Kept(e) => warn!(worker = w.name, "worktree kept: {e}"),
    }
    cleanup
}

#[derive(Debug, PartialEq, Eq)]
enum WorktreeCleanup {
    Removed,
    /// git refused because it has uncommitted or untracked changes.
    KeptDirty,
    /// git refused for another reason (locked, has submodules, already gone, ...).
    Kept(String),
}

impl WorktreeCleanup {
    /// For the bot. Not git's own text, which suggests `--force`.
    fn note(&self) -> &'static str {
        match self {
            Self::Removed => "Worktree removed",
            Self::KeptDirty => "Worktree kept: it has uncommitted changes",
            Self::Kept(_) => "Worktree kept: git would not remove it",
        }
    }
}

/// `git worktree remove` without `--force`, so git refuses a worktree with changes in it.
/// The branch is never deleted: its commits stay in the repo.
async fn remove_worktree_if_clean(repo: &Path, worktree: &Path) -> WorktreeCleanup {
    let Err(e) = git(repo, &["worktree", "remove", &worktree.to_string_lossy()]).await else {
        return WorktreeCleanup::Removed;
    };
    let dirty = git(worktree, &["status", "--porcelain"])
        .await
        .is_ok_and(|changes| !changes.trim().is_empty());
    if dirty {
        WorktreeCleanup::KeptDirty
    } else {
        WorktreeCleanup::Kept(format!("{e:#}"))
    }
}

fn worker_message(spec: &str, branch: &str, name: &str, exe: &Path) -> String {
    let exe = exe.display();
    format!(
        "{spec}\n\n[herdr-bots worker {name}] Rules: work only in this worktree (branch `{branch}`). \
         Verify your change (build/tests). Commit, push this branch, and open a PR with `gh pr create`. \
         Never push to main/master or force-push. When done (or stuck), run \
         `{exe} worker-done {name} --pr <url or -> --summary \"<what you did, what's left>\"` and end your turn."
    )
}

fn worktrees_dir(app: &App) -> PathBuf {
    match &app.config.worktrees_dir {
        Some(dir) => expand_home(dir),
        None => expand_home("~/.herdr/worktrees"),
    }
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

async fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await
        .context("could not run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_has_branch_and_report_command() {
        let m = worker_message("Fix it", "bot/a-1-1", "w1-1", Path::new("/bin/herdr-bots"));
        assert!(m.starts_with("Fix it"));
        assert!(m.contains("`bot/a-1-1`"));
        assert!(m.contains("/bin/herdr-bots worker-done w1-1"));
    }

    const AS_TESTER: [&str; 4] = ["-c", "user.email=t@t", "-c", "user.name=t"];

    async fn repo_with_worktree(tmp: &Path, branch: &str) -> (PathBuf, PathBuf) {
        let repo = tmp.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]).await.unwrap();
        let init = [
            &AS_TESTER[..],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ]
        .concat();
        git(&repo, &init).await.unwrap();
        let worktree = tmp.join("worktree");
        let path = worktree.to_string_lossy();
        git(&repo, &["worktree", "add", "-q", "-b", branch, &path])
            .await
            .unwrap();
        (repo, worktree)
    }

    #[tokio::test]
    async fn a_clean_worktree_is_removed_and_its_branch_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, worktree) = repo_with_worktree(tmp.path(), "bot/a-1-1").await;
        std::fs::write(worktree.join("fix.txt"), "fixed").unwrap();
        git(&worktree, &["add", "fix.txt"]).await.unwrap();
        let commit = [&AS_TESTER[..], &["commit", "-q", "-m", "the fix"]].concat();
        git(&worktree, &commit).await.unwrap();

        assert_eq!(
            remove_worktree_if_clean(&repo, &worktree).await,
            WorktreeCleanup::Removed
        );
        assert!(!worktree.exists());
        let log = git(&repo, &["log", "--format=%s", "bot/a-1-1"])
            .await
            .unwrap();
        assert!(
            log.contains("the fix"),
            "branch kept with its commit: {log}"
        );
    }

    #[tokio::test]
    async fn a_worktree_with_uncommitted_work_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, worktree) = repo_with_worktree(tmp.path(), "bot/a-1-1").await;
        std::fs::write(worktree.join("wip.txt"), "not committed").unwrap();

        let cleanup = remove_worktree_if_clean(&repo, &worktree).await;
        assert_eq!(cleanup, WorktreeCleanup::KeptDirty);
        assert_eq!(cleanup.note(), "Worktree kept: it has uncommitted changes");
        assert!(worktree.join("wip.txt").exists());
    }

    #[test]
    fn home_expands() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~/x"), Path::new(&home).join("x"));
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
    }
}
