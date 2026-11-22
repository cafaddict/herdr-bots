use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::Value;

use herdr_bots::config::{AgentKind, Permission, valid_bot_id};
use herdr_bots::ipc::{self, Request};
use herdr_bots::paths::Paths;
use herdr_bots::{daemon, service, templates};

/// Bots for herdr: agents with a soul, a memory and a work queue.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground (the service manager does this for you).
    Daemon,
    /// Make a new bot folder in ~/.herdr-bots/bots/<id>.
    New {
        id: String,
        #[arg(long, value_enum, default_value = "claude")]
        agent: AgentArg,
        #[arg(long, value_enum, default_value = "full-access")]
        permission: PermissionArg,
        /// One line for SOUL.md: what this bot is for.
        #[arg(long)]
        role: Option<String>,
    },
    /// Message a bot. With --item, answers that item's question.
    Ask {
        bot: String,
        text: String,
        #[arg(long)]
        item: Option<i64>,
    },
    /// Bots, open items and workers.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// For bots: ask the owner a question and park the item.
    NeedsYou {
        item: i64,
        question: String,
    },
    /// Mark an item done.
    Done {
        item: i64,
    },
    /// Cancel an item.
    Cancel {
        item: i64,
    },
    /// Stop a bot's current turn and hold its queue.
    Pause {
        bot: String,
    },
    Resume {
        bot: String,
    },
    /// Re-read every bot.toml now.
    Reload,
    /// For bots: hand a code change to a worker in a new git worktree.
    WorkerStart {
        #[arg(long)]
        item: i64,
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        spec: String,
        /// Branch or commit to start from (default: the repo's current HEAD).
        #[arg(long)]
        base: Option<String>,
    },
    /// For workers: report the result (the daemon notices when the worker's turn ends).
    WorkerDone {
        name: String,
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        summary: String,
    },
    /// Register the daemon with launchd (macOS) or systemd --user (Linux) and start it.
    InstallService,
    UninstallService,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum AgentArg {
    Claude,
    Codex,
}

impl From<AgentArg> for AgentKind {
    fn from(a: AgentArg) -> Self {
        match a {
            AgentArg::Claude => AgentKind::Claude,
            AgentArg::Codex => AgentKind::Codex,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum PermissionArg {
    FullAccess,
    AcceptEdits,
    Ask,
}

impl From<PermissionArg> for Permission {
    fn from(p: PermissionArg) -> Self {
        match p {
            PermissionArg::FullAccess => Permission::FullAccess,
            PermissionArg::AcceptEdits => Permission::AcceptEdits,
            PermissionArg::Ask => Permission::Ask,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    match cli.command {
        Command::Daemon => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with_ansi(false)
                .init();
            daemon::run(paths).await
        }
        Command::New {
            id,
            agent,
            permission,
            role,
        } => {
            let new = templates::NewBot {
                agent: agent.into(),
                permission: permission.into(),
                role: role.as_deref(),
            };
            new_bot(&paths, &id, &new).await
        }
        Command::Status { json } => {
            let data = ipc::call(&paths.socket(), &Request::Status).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                print_status(&data);
            }
            Ok(())
        }
        Command::InstallService => {
            let file = service::install(&paths)?;
            // The service manager starts it asynchronously; don't report success until it answers.
            for _ in 0..50 {
                if ipc::call(&paths.socket(), &Request::Ping).await.is_ok() {
                    println!("installed {} and the daemon is up", file.display());
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            anyhow::bail!(
                "installed {} but the daemon didn't answer within 5 s; see {}",
                file.display(),
                paths.logs_dir().join("daemon.log").display()
            )
        }
        Command::UninstallService => service::uninstall(),
        other => {
            let req = match other {
                Command::Ask { bot, text, item } => Request::Ask { bot, text, item },
                Command::NeedsYou { item, question } => Request::NeedsYou { item, question },
                Command::Done { item } => Request::Done { item },
                Command::Cancel { item } => Request::Cancel { item },
                Command::Pause { bot } => Request::Pause { bot },
                Command::Resume { bot } => Request::Resume { bot },
                Command::Reload => Request::Reload,
                Command::WorkerStart {
                    item,
                    repo,
                    spec,
                    base,
                } => {
                    let repo = std::fs::canonicalize(&repo)
                        .with_context(|| format!("{}", repo.display()))?;
                    Request::WorkerStart {
                        item,
                        repo,
                        spec,
                        base,
                    }
                }
                Command::WorkerDone { name, pr, summary } => {
                    Request::WorkerDone { name, pr, summary }
                }
                _ => unreachable!("handled above"),
            };
            let data = ipc::call(&paths.socket(), &req).await?;
            println!("{}", serde_json::to_string(&data)?);
            Ok(())
        }
    }
}

async fn new_bot(paths: &Paths, id: &str, new: &templates::NewBot<'_>) -> anyhow::Result<()> {
    anyhow::ensure!(valid_bot_id(id), "bot ids are 1-24 of a-z 0-9 - _");
    paths.ensure()?;
    let home = paths.bot_home(id);
    templates::create_bot_home(&home, id, new)?;
    println!("made {}", home.display());
    println!("edit SOUL.md (who it is) and bot.toml (agent, permission, schedule, triggers)");
    if ipc::call(&paths.socket(), &Request::Reload).await.is_err() {
        println!("(daemon not running; it picks the bot up when it starts)");
    }
    Ok(())
}

fn print_status(data: &Value) {
    let empty = Vec::new();
    let list = |k: &str| {
        data.get(k)
            .and_then(Value::as_array)
            .unwrap_or(&empty)
            .clone()
    };
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    println!("bots:");
    for b in list("bots") {
        let paused = if b["paused"].as_bool() == Some(true) {
            " (paused)"
        } else {
            ""
        };
        println!(
            "  {:<16} {:<10} {}{paused}",
            s(&b, "id"),
            s(&b, "state"),
            s(&b, "agent")
        );
    }
    if let Some(errors) = data.get("bot_errors").and_then(Value::as_object) {
        for (id, e) in errors {
            println!("  {id:<16} BROKEN     {}", e.as_str().unwrap_or(""));
        }
    }
    println!("items:");
    for i in list("items").iter().take(15) {
        let q = i
            .get("question")
            .and_then(Value::as_str)
            .map(|q| format!("  ? {q}"))
            .unwrap_or_default();
        println!(
            "  #{:<5} {:<10} {:<12} {}{q}",
            i["id"],
            s(i, "status"),
            s(i, "bot"),
            s(i, "title")
        );
    }
    let workers = list("workers");
    if !workers.is_empty() {
        println!("workers:");
        for w in workers.iter().take(10) {
            println!(
                "  {:<10} {:<8} #{} {}",
                s(w, "name"),
                s(w, "status"),
                w["item_id"],
                s(w, "branch")
            );
        }
    }
}
