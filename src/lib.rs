//! herdr-bots: bots in the style of Orca Bots, run on top of herdr-core (`hc`).
//!
//! A bot is a folder (`SOUL.md`, `MEMORY.md`, `bot.toml`) plus an interactive agent session in
//! an hc terminal named `bot-<id>`. The daemon files work items from chat, schedules, GitHub and
//! Linear, feeds them to the bot one turn at a time, and hands code changes to workers in their
//! own git worktrees.

pub mod agent;
pub mod app;
pub mod config;
pub mod daemon;
pub mod hc;
pub mod ipc;
pub mod paths;
pub mod runner;
pub mod schedule;
pub mod service;
pub mod store;
pub mod templates;
pub mod triggers;
pub mod worker;
