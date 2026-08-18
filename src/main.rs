//! `shep` — one binary. `shep supervise` is the daemon; every other subcommand
//! is a client that writes a transaction and lets the supervisor notice on its
//! next poll. There is no socket and no server (PLAN §7.4).

use anyhow::Result;
use clap::{Parser, Subcommand};
use shepherd::{cmd, logging, paths};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "shep",
    version,
    about = "Agent orchestration over Herdr",
    long_about = "Agent orchestration over Herdr.\n\nThe CLI and the supervisor are the same \
                  binary over the same SQLite store: a command is a transaction, and the \
                  supervisor picks it up on its next poll."
)]
struct Cli {
    /// The store to act on. Defaults to $SHEP_DB, else <state dir>/shep.db.
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Queue a task. Prints its id.
    Create {
        /// Which composition of pipelines to run.
        #[arg(long = "type", value_name = "TYPE")]
        kind: String,
        /// The repo whose .shep/config.toml governs this task. Defaults to the
        /// current repo root.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        /// What to do.
        #[arg(value_name = "BRIEF", required = true, num_args = 1..)]
        brief: Vec<String>,
    },

    /// Run the supervisor: poll the store, spawn steps, resolve outcomes.
    Supervise {
        /// Poll interval in milliseconds.
        #[arg(long, default_value_t = 200, value_name = "MS")]
        poll_ms: u64,
        /// Stop after this many ticks. For testing.
        #[arg(long, value_name = "N")]
        ticks: Option<u64>,
    },

    /// Is anything driving this store, and what is in it?
    Status {
        #[arg(long)]
        json: bool,
    },

    /// List tasks.
    Ps {
        /// Include finished and cancelled tasks.
        #[arg(long, short)]
        all: bool,
        #[arg(long)]
        json: bool,
    },

    /// Stop the supervisor starting anything new.
    Pause,

    /// Undo `shep pause`.
    Resume,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.clone().unwrap_or_else(paths::db_path);

    match cli.command {
        // The supervisor sets up its own file logging; everything else is a
        // one-shot command that should stay quiet on stderr.
        Command::Supervise { poll_ms, ticks } => cmd::supervise::run(&db, poll_ms, ticks),
        other => {
            logging::init_cli();
            match other {
                Command::Create { kind, repo, brief } => cmd::create::run(&db, &kind, repo, &brief),
                Command::Status { json } => cmd::status::run(&db, json),
                Command::Ps { all, json } => cmd::ps::run(&db, all, json),
                Command::Pause => cmd::pause::run(&db, true),
                Command::Resume => cmd::pause::run(&db, false),
                Command::Supervise { .. } => unreachable!("handled above"),
            }
        }
    }
}
