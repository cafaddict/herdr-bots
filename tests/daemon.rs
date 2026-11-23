//! End to end: the real daemon binary, a fake `hc`, a temporary home.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_herdr-bots");

struct Daemon {
    child: Child,
    root: PathBuf,
    fake: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn start(tmp: &Path) -> Self {
        Self::start_with(tmp, "")
    }

    fn start_with(tmp: &Path, extra_config: &str) -> Self {
        let root = tmp.join("home");
        let fake = tmp.join("hc-state");
        std::fs::create_dir_all(&root).unwrap();
        let hc = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-hc.sh");
        // `gh = "false"`: triggers fail fast and quietly; this test is about chat and schedules.
        std::fs::write(
            root.join("config.toml"),
            format!(
                "hc = \"{}\"\ngh = \"false\"\npoll_seconds = 3600\n{extra_config}",
                hc.display()
            ),
        )
        .unwrap();
        let child = Command::new(BIN)
            .arg("daemon")
            .env("HERDR_BOTS_HOME", &root)
            .env("FAKE_HC_DIR", &fake)
            .env("HERDR_BOTS_BIN", BIN)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let d = Self { child, root, fake };
        d.until("daemon socket", || d.cli(&["status", "--json"]).is_ok());
        d
    }

    fn cli(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(BIN)
            .args(args)
            .env("HERDR_BOTS_HOME", &self.root)
            .output()
            .unwrap();
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }

    fn status(&self) -> Value {
        serde_json::from_str(&self.cli(&["status", "--json"]).unwrap()).unwrap()
    }

    fn item(&self, id: i64) -> Value {
        self.status()["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn until(&self, what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }
}

#[test]
fn chat_turn_runs_and_collects_the_outbox() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Daemon::start(tmp.path());
    d.cli(&["new", "echo"]).unwrap();
    d.until("bot loaded", || {
        d.status()["bots"].as_array().is_some_and(|b| b.len() == 1)
    });
    let home = d.root.join("bots/echo");
    assert!(
        home.join("HERDR-BOTS.md").is_file(),
        "rules written on load"
    );

    let filed: Value =
        serde_json::from_str(&d.cli(&["ask", "echo", "hello there"]).unwrap()).unwrap();
    let id = filed["id"].as_i64().unwrap();
    d.until("item done", || d.item(id)["status"] == "done");
    assert_eq!(
        d.item(id)["reply"].as_str().unwrap().trim(),
        format!("fake reply {id}")
    );

    // The agent was started in the bot's home with full-access flags, then given the message.
    let term = d.fake.join("bot-echo");
    assert_eq!(
        std::fs::read_to_string(term.join("cwd")).unwrap(),
        home.to_string_lossy()
    );
    assert!(
        std::fs::read_to_string(term.join("argv"))
            .unwrap()
            .contains("claude --dangerously-skip-permissions")
    );
    assert!(
        std::fs::read_to_string(term.join("log"))
            .unwrap()
            .contains("[herdr-bots item #")
    );

    // status.json for the panel follows.
    let snap = d.root.join("state/status.json");
    d.until("status.json", || {
        std::fs::read_to_string(&snap).is_ok_and(|s| s.contains("\"done\""))
    });
}

#[test]
fn needs_you_then_answer_reruns_the_item() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Daemon::start(tmp.path());
    d.cli(&["new", "ops"]).unwrap();
    d.until("bot loaded", || {
        !d.status()["bots"].as_array().unwrap().is_empty()
    });

    // Paused: the item is filed but no turn runs.
    d.cli(&["pause", "ops"]).unwrap();
    let filed: Value =
        serde_json::from_str(&d.cli(&["ask", "ops", "ASKME deploy"]).unwrap()).unwrap();
    let id = filed["id"].as_i64().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(d.item(id)["status"], "planned");
    // planned -> needs_you is refused: a bot only asks during its turn.
    assert!(d.cli(&["needs-you", &id.to_string(), "?"]).is_err());

    // Resumed: the fake bot asks a question instead of answering.
    d.cli(&["resume", "ops"]).unwrap();
    d.until("question", || d.item(id)["status"] == "needs_you");
    assert_eq!(d.item(id)["question"], "which env?");

    // My answer goes back to the same item and a second turn finishes it.
    d.cli(&["ask", "ops", "staging", "--item", &id.to_string()])
        .unwrap();
    d.until("answered turn done", || d.item(id)["status"] == "done");
    let log = std::fs::read_to_string(d.fake.join("bot-ops/log")).unwrap();
    assert!(log.contains("[answer] staging"));
    // A finished item can't be answered again.
    assert!(
        d.cli(&["ask", "ops", "again", "--item", &id.to_string()])
            .is_err()
    );
}

#[test]
fn every_schedule_files_items() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Daemon::start(tmp.path());
    d.cli(&["new", "tick"]).unwrap();
    // Not a real minute: patch the parsed schedule through the config file with the shortest
    // allowed interval and check that the next run is computed and shown.
    let toml = d.root.join("bots/tick/bot.toml");
    std::fs::write(
        &toml,
        "[[schedule]]\nwhen = \"every 1m\"\nprompt = \"tick\"\n",
    )
    .unwrap();
    d.cli(&["reload"]).unwrap();
    let next = d.status()["bots"][0]["schedule"][0]["next"]
        .as_str()
        .map(str::to_owned);
    assert!(next.is_some(), "next run shown");
    // Broken config shows up as an error instead of killing the daemon.
    std::fs::write(&toml, "[[schedule]]\nwhen = \"every\"\nprompt = \"x\"\n").unwrap();
    d.cli(&["reload"]).unwrap();
    assert!(d.status()["bot_errors"]["tick"].is_string());
}

#[test]
fn second_daemon_refuses_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Daemon::start(tmp.path());
    let out = Command::new(BIN)
        .arg("daemon")
        .env("HERDR_BOTS_HOME", &d.root)
        .env("FAKE_HC_DIR", &d.fake)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(d.cli(&["status"]).is_ok(), "the first one keeps running");
}

#[test]
fn worker_gets_its_own_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);

    let wt = tmp.path().join("worktrees");
    let d = Daemon::start_with(
        tmp.path(),
        &format!("worktrees_dir = \"{}\"\n", wt.display()),
    );
    d.cli(&["new", "dev"]).unwrap();
    d.until("bot loaded", || {
        !d.status()["bots"].as_array().unwrap().is_empty()
    });
    d.cli(&["pause", "dev"]).unwrap();
    let filed: Value = serde_json::from_str(&d.cli(&["ask", "dev", "fix it"]).unwrap()).unwrap();
    let id = filed["id"].as_i64().unwrap().to_string();

    let w: Value = serde_json::from_str(
        &d.cli(&[
            "worker-start",
            "--item",
            &id,
            "--repo",
            repo.to_str().unwrap(),
            "--spec",
            "do the fix",
        ])
        .unwrap(),
    )
    .unwrap();
    let branch = format!("bot/dev-{id}-1");
    assert_eq!(w["branch"], branch.as_str());
    let worktree = PathBuf::from(w["worktree"].as_str().unwrap());
    assert!(worktree.starts_with(&wt));
    d.until("worker finished", || {
        d.status()["workers"][0]["status"] == "done"
    });
    // Its terminal is ended and its clean worktree removed; the branch stays.
    let name = format!("w{id}-1");
    assert!(!d.fake.join(&name).exists(), "terminal removed");
    let terminal = d.fake.join(".removed").join(&name);
    let cwd = std::fs::read_to_string(terminal.join("cwd")).unwrap();
    assert_eq!(Path::new(&cwd), worktree);
    let log = std::fs::read_to_string(terminal.join("log")).unwrap();
    assert!(log.contains("do the fix") && log.contains(&format!("worker-done {name}")));
    assert!(!worktree.exists(), "clean worktree removed");
    git(&[
        "rev-parse",
        "--verify",
        "-q",
        &format!("refs/heads/{branch}"),
    ]);
    // A finished item can't get workers.
    d.cli(&["cancel", &id]).unwrap();
    assert!(
        d.cli(&[
            "worker-start",
            "--item",
            &id,
            "--repo",
            repo.to_str().unwrap(),
            "--spec",
            "x"
        ])
        .is_err()
    );
}

#[test]
fn rm_cancels_items_and_moves_the_bot_to_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Daemon::start(tmp.path());
    d.cli(&["new", "gone"]).unwrap();
    d.until("bot loaded", || {
        !d.status()["bots"].as_array().unwrap().is_empty()
    });
    d.cli(&["pause", "gone"]).unwrap();
    let filed: Value = serde_json::from_str(&d.cli(&["ask", "gone", "later"]).unwrap()).unwrap();
    let id = filed["id"].as_i64().unwrap();
    std::fs::write(d.root.join("bots/gone/MEMORY.md"), "- keep me\n").unwrap();

    let out: Value = serde_json::from_str(&d.cli(&["rm", "gone"]).unwrap()).unwrap();
    assert_eq!(out["canceled_items"], 1);
    assert_eq!(d.item(id)["status"], "canceled");
    assert!(!d.root.join("bots/gone").exists());
    let trash = PathBuf::from(out["trash"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(trash.join("MEMORY.md")).unwrap(),
        "- keep me\n"
    );
    assert!(d.status()["bots"].as_array().unwrap().is_empty());
    assert!(d.cli(&["rm", "gone"]).is_err(), "already gone");
    assert!(d.cli(&["rm", "../etc"]).is_err(), "not a bot id");
    // The same name can be used again.
    d.cli(&["new", "gone"]).unwrap();
}

#[test]
fn rename_moves_the_folder_and_its_items() {
    let tmp = tempfile::tempdir().unwrap();
    let d = Daemon::start(tmp.path());
    d.cli(&["new", "old"]).unwrap();
    d.cli(&["new", "taken"]).unwrap();
    d.until("bots loaded", || {
        d.status()["bots"].as_array().unwrap().len() == 2
    });
    d.cli(&["pause", "old"]).unwrap();
    let filed: Value = serde_json::from_str(&d.cli(&["ask", "old", "later"]).unwrap()).unwrap();
    let id = filed["id"].as_i64().unwrap();
    std::fs::write(d.root.join("bots/old/MEMORY.md"), "- keep me\n").unwrap();

    assert!(d.cli(&["rename", "old", "taken"]).is_err(), "name in use");
    assert!(d.cli(&["rename", "old", "../etc"]).is_err(), "not a bot id");
    assert!(d.cli(&["rename", "nobody", "x"]).is_err(), "no such bot");

    let out: Value = serde_json::from_str(&d.cli(&["rename", "old", "new"]).unwrap()).unwrap();
    assert_eq!(out["to"], "new");
    assert!(!d.root.join("bots/old").exists());
    assert_eq!(
        std::fs::read_to_string(d.root.join("bots/new/MEMORY.md")).unwrap(),
        "- keep me\n"
    );
    assert_eq!(d.item(id)["bot"], "new", "its item follows");
    d.until("renamed bot loaded", || {
        let ids: Vec<String> = d.status()["bots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].as_str().unwrap().to_string())
            .collect();
        ids.contains(&"new".to_string()) && !ids.contains(&"old".to_string())
    });
    // Paused stays paused under the new name.
    let bot = d.status()["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "new")
        .cloned()
        .unwrap();
    assert_eq!(bot["paused"], true);
}
