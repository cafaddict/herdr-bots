# herdr-bots

Bots for [Herdr](https://herdr.dev): coding agents with a soul, a memory and a work queue,
in the spirit of Orca Bots, but running entirely on your own machine on top of herdr-core
(`hc`).

A bot is a folder plus a long-lived, interactive Claude Code or Codex session in an hc terminal.
A small Rust daemon brings work to it: from a Herdr panel, on a schedule, or from GitHub and
Linear. The daemon feeds the work to the bot one turn at a time and collects its answers. The
bot hands code changes to **workers**, which are agents in their own git worktrees that open PRs
and report back.

```
 Herdr panel ──┐                         ┌── bot-<id>   (claude/codex in ~/.herdr-bots/bots/<id>)
 schedule ─────┤   herdr-bots daemon     │      │ herdr-bots worker-start
 GitHub (gh) ──┼──► items (SQLite) ──────┤      ▼
 Linear ───────┘   one turn at a time    └── w<item>-<n> (agent in ~/.herdr/worktrees/<repo>/bot/...)
```

## Install

You need Rust (`cargo`) and Herdr, which provides `hc`. `gh` is optional and only needed for
GitHub triggers.

```bash
git clone https://github.com/cafaddict/herdr-bots && cd herdr-bots
./install.sh
```

`install.sh` does these things, and running it again just updates each one:

1. Builds and installs the `herdr-bots` binary (`cargo install`).
2. Copies the panel mod to `~/.config/herdr-app/mods/bots`.
3. Registers the daemon as a service that starts at login and restarts if it dies:
   launchd (`~/Library/LaunchAgents/dev.herdr-bots.plist`) on macOS, `systemd --user` on Linux.
4. Adds **Bots** to a copy of Herdr's `main` layout called `main-bots` and switches to it.
   Herdr's own layouts are read-only, which is why it works on a copy.
   - The left sidebar gets a list of bots, above Agents.
   - The title bar gets a **Bots** button next to Files and Changes. It opens the right panel
     with a bot's chat: its items, its questions to you, its answers, and a message box.
     Clicking a bot in the sidebar opens the same panel on that bot. **Terminal** opens the live
     Claude Code / Codex session.
   - The Bots button lives in `tabs-bots`, a copy of Herdr's title bar mod with that one button
     added. `install.sh` makes a fresh copy every time it runs, so rerun it after Herdr updates.
     See `mod/setup_layout.py`.
   - **+** in the sidebar header makes a new bot. In the Bots panel, **‹ Bots** goes back to
     the list of bots, which has **+ New bot**.
   `main-bots` is your layout now: Herdr updates to `main` don't reach it. To go back, run
   `herdr-app do layout.switch '{"name":"main"}'`.

"All" in the sidebar header opens everything at once: every bot, item and running worker. The
same view also opens with `herdr-app do mod.open '{"name":"bots"}'`.

## A bot

Click **+** in the sidebar's Bots header. Or, from a shell:

```bash
herdr-bots new scout --agent claude --permission full-access --role "Reviews my PRs"
```

This creates `~/.herdr-bots/bots/scout/`:

| File | What it holds |
|---|---|
| `SOUL.md` | Who the bot is and what it does. Write this yourself. |
| `MEMORY.md`, `memory/` | What the bot remembers, one line per fact. The bot keeps this up to date. |
| `bot.toml` | Which agent, which permission mode, the schedule and the triggers. |
| `outbox/<item>.md` | The bot's answer for each item. The panel shows this. |
| `HERDR-BOTS.md` | How to work with the daemon. Rewritten on every daemon start. |
| `CLAUDE.md`, `AGENTS.md` | Make Claude Code and Codex load the three files above. |

`bot.toml`. Every key is optional:

```toml
agent = "claude"              # claude | codex
permission = "full-access"    # full-access | accept-edits | ask
model = "opus"

[[schedule]]
when = "30 8 * * 1-5"         # cron, local time; or "every 30m"
prompt = "Summarize what changed in my repos since yesterday."

[github]
repos = ["owner/repo"]        # empty = everywhere
events = ["review-requested", "assigned", "ci-failed"]

[linear]
team = "ENG"

allow_authors = ["teammate"]  # who may trigger this bot from GitHub/Linear
```

To delete a bot, hover it in the Bots list (sidebar or panel) and click **Delete**, click **Delete** in its chat, or run `herdr-bots rm <id>`. Deleting does three
things:

- Cancels the bot's open items.
- Ends its agent's terminal.
- Moves its folder to `~/.herdr-bots/trash/<id>-<time>`. To bring the bot back, move the folder
  back into `bots/<id>`.

If the bot has workers running, `rm` refuses. `--force` (or the checkbox in the dialog) stops the
workers too. Each stopped worker is then cleaned up like a finished one (see the worker step in
[How a turn works](#how-a-turn-works)): its worktree is removed if it is clean and kept if it has
uncommitted changes. Its branch stays.

The daemon reloads a bot within 5 seconds after you save its `bot.toml`. A broken `bot.toml`
shows up as an error in the panel and in `herdr-bots status` instead of stopping the daemon.

## How a turn works

1. Something files an **item** for a bot: a panel message, a schedule, or a GitHub/Linear event.
   An item moves through these states: `planned` → `working` → `done`, or → `needs_you`.
2. The bot's terminal `bot-<id>` starts if it isn't running. It is an interactive session that a
   person can attach to and take over at any time.
   - The first time an agent starts in a folder the daemon made, the daemon answers the
     **folder-trust** dialog itself.
   - Claude's one-time "bypass permissions" consent is not answered for you. It becomes a
     `needs_you` item: open the bot from the panel and answer it there.
3. The daemon pastes in the item and waits for the turn to end, using `hc send --wait`.
4. The bot ends its turn in one of three ways:
   - It writes its answer to `outbox/<n>.md`, and the item becomes `done`.
   - It runs `herdr-bots needs-you <n> "question"`. You answer in the panel, and the item runs again.
   - It runs `herdr-bots worker-start --item <n> --repo <path> --spec "..."`. That command
     creates the branch `bot/<bot>-<n>-<k>` in a new worktree and starts an agent there. When the
     worker's turn ends, the daemon ends the worker's terminal and removes its worktree if it is
     clean (`git worktree remove` without `--force`). A worktree with uncommitted or untracked
     changes is kept, and the branch is never deleted. Then the worker's report (PR link and
     summary from `herdr-bots worker-done`, and whether the worktree was removed) comes back to
     the bot as a new turn on the same item. A worker stuck on a permission dialog keeps its
     terminal and worktree, so you can answer the dialog there.

A bot runs one turn at a time. Workers run in parallel.

## Triggers

- **Schedule:** cron or `every <n><s|m|h|d>` (one minute at least). A run that falls while the
  machine is asleep is skipped, not caught up.
- **GitHub:** polled through `gh` every `poll_seconds` (default 120):
  - `review-requested`: a PR asks for your review.
  - `assigned`: an open issue is assigned to you.
  - `ci-failed`: a check failed on the current head of one of your open PRs. A new push can fail again.
- **Linear:** issues assigned to you. The API key comes from the first of these that exists:
  `$LINEAR_API_KEY`, the macOS Keychain item `herdr-bots-linear`
  (`security add-generic-password -s herdr-bots-linear -a me -w <key>`), or
  `~/.herdr-bots/secrets/linear` (chmod 600).

The first poll after you turn a trigger on only records what already exists. Only things that
appear after that become items.

## Safety

`full-access` means the agent never asks before it acts. That is the default because a bot
mostly works unattended, so the defaults try to limit who can give it work:

- **Allow list.** GitHub and Linear items are filed only when their author is on the allow list:
  the bot's `allow_authors`, else the daemon's `allow_authors` in `config.toml`, else only you
  (`gh api user`). Linear accepts anyone in your workspace unless the list contains emails.
- **Fenced text.** Text from GitHub and Linear reaches the bot inside `<external>` tags, and the
  bot's rules say it is data, not instructions.
- **Push rules.** Workers are told never to push to main/master or force-push, and to open PRs.
- **Stopping.** Pause a bot from the panel or with `herdr-bots pause <id>`: this interrupts its
  current turn and holds its queue.

If you don't want that, set `permission = "accept-edits"` or `"ask"` in a bot's `bot.toml`.

## Commands

```
herdr-bots status [--json]         bots, open items, workers
herdr-bots ask <bot> <text> [--item N]
herdr-bots done|cancel <item>
herdr-bots pause|resume <bot>
herdr-bots rm <bot> [--force]      delete a bot (its folder goes to ~/.herdr-bots/trash)
herdr-bots reload
herdr-bots install-service | uninstall-service
herdr-bots daemon                  (what the service runs)
```

Bots and workers call `needs-you`, `worker-start` and `worker-done` themselves.

## Files

All state lives in `~/.herdr-bots`. Set `$HERDR_BOTS_HOME` to use another folder.

- `bots/<id>/`: one folder per bot (see above).
- `state/state.db`: items, item history, workers, trigger cursors (SQLite).
- `state/status.json`: what the panel reads.
- `botsd.sock`: the control socket. Mode 0600. Set `$HERDR_BOTS_SOCKET` to put it elsewhere.
- `logs/daemon.log`
- `config.toml` (optional), with its defaults:

```toml
hc = "hc"
gh = "gh"
poll_seconds = 120
allow_authors = []
# worktrees_dir = "~/.herdr/worktrees"
```

## Development

```bash
cargo test                    # unit tests + the daemon end to end against tests/fake-hc.sh
cargo clippy --all-targets -- -D warnings
herdr-app check mod/bots
```

License: MIT
