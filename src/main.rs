//! `shep` — one binary. `shep supervise` is the daemon; every other subcommand
//! is a client that writes a transaction and lets the supervisor notice on its
//! next poll. There is no socket and no server.

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

    /// Everything the store knows about one task.
    Get {
        #[arg(value_name = "TASK")]
        task: String,
        #[arg(long)]
        json: bool,
    },

    /// What happened to a task, and what each thing led to.
    Trace {
        #[arg(value_name = "TASK")]
        task: String,
        #[arg(long)]
        json: bool,
    },

    /// Bind a Herdr pane to a task, with the worktree its work happens in.
    /// Run by a step script once it has a pane, before it starts an agent there.
    #[command(name = "bind-pane")]
    BindPane {
        #[arg(value_name = "PANE")]
        pane: String,
        /// Defaults to $SHEP_TASK_ID, else the task bound to $HERDR_PANE_ID.
        #[arg(long, value_name = "TASK")]
        task: Option<String>,
        /// The Herdr workspace the pane lives in. Recorded because
        /// `workspace.closed` carries no pane id.
        #[arg(long, value_name = "ID")]
        workspace: Option<String>,
        #[arg(long, value_name = "PATH")]
        worktree: Option<String>,
        #[arg(long, value_name = "NAME")]
        branch: Option<String>,
        #[arg(long, value_name = "REF")]
        base: Option<String>,
    },

    /// Print the brief. What an agent in a pane runs to find out what it is for.
    Context {
        /// Defaults to $SHEP_TASK_ID, else the task bound to $HERDR_PANE_ID.
        #[arg(long, value_name = "TASK")]
        task: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Verdicts about a commit.
    Check {
        #[command(subcommand)]
        action: CheckCommand,
    },

    /// Print one addressed artefact, such as a check.
    Read {
        #[arg(value_name = "ID")]
        id: String,
    },

    /// Resolve a step that is awaiting a named signal (CI, a webhook, a script).
    Signal {
        /// The signal name the step awaits, e.g. a declared `[signal.ci]`.
        #[arg(long, value_name = "NAME")]
        name: String,
        #[arg(long, conflicts_with = "fail")]
        pass: bool,
        #[arg(long)]
        fail: bool,
        #[arg(long, value_name = "TASK")]
        task: Option<String>,
        /// Who/what is signalling. Defaults to the signal name.
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        /// Why, for the record. Also read from stdin when piped.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },

    /// Send a task back through one of its type's pipelines, by hand.
    Run {
        #[arg(value_name = "PIPELINE")]
        pipeline: String,
        #[arg(long, value_name = "TASK")]
        task: Option<String>,
    },

    /// Re-queue a parked task, retrying the step it stopped on.
    Retry {
        #[arg(value_name = "TASK")]
        task: String,
    },

    /// Stop a task for good.
    Cancel {
        #[arg(value_name = "TASK")]
        task: String,
        /// Why, for the audit trail.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Print the orchestrator skill: everything a conversational agent needs
    /// to know to create, watch and settle tasks. Markdown on stdout, with
    /// this repo's task types read live from its config.
    Skill {
        /// The repo whose policy to read. Defaults to the current repo root.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        /// Print the orchestrating skill: creating, watching and settling
        /// tasks. This is the default.
        #[arg(long, conflicts_with = "authoring")]
        orchestrating: bool,
        /// Print the workflow-authoring skill instead: the config schema, the
        /// script contract, and the design rules, for an agent writing .shep/.
        #[arg(long)]
        authoring: bool,
    },

    /// Scaffold .shep/ in this repo: a minimal valid config and stub scripts.
    /// Never overwrites; existing files are kept and the gaps filled.
    Init {
        /// The repo to scaffold. Defaults to the current repo root.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
    },

    /// List the types a task can be created as, with their descriptions.
    Types {
        /// The repo whose policy to read. Defaults to the current repo root.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Check this repo's .shep/config.toml and report every problem with it.
    Validate {
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Append a Herdr event to the store. Run by hooks/forward.sh; the event
    /// arrives in $HERDR_PLUGIN_EVENT_JSON.
    Forward,

    /// Show what Herdr has told us, newest last.
    Raw {
        #[arg(long, short, default_value_t = 20, value_name = "N")]
        limit: i64,
        #[arg(long)]
        json: bool,
    },

    /// Stop the supervisor starting anything new.
    Pause,

    /// Undo `shep pause`.
    Resume,
}

#[derive(Subcommand)]
enum CheckCommand {
    /// Record a verdict about the worktree as it stands. Body on stdin.
    ///
    /// `shep` stamps the sha itself, so a check always says which commit it
    /// judged.
    Submit {
        #[arg(long, conflicts_with = "fail")]
        pass: bool,
        #[arg(long)]
        fail: bool,
        /// Who is judging. Defaults to the step name.
        #[arg(long, value_name = "NAME")]
        author: Option<String>,
        /// Defaults to $SHEP_TASK_ID, else the task bound to $HERDR_PANE_ID.
        #[arg(long, value_name = "TASK")]
        task: Option<String>,
    },
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
                Command::Types { repo, json } => cmd::types::run(&cmd::repo_root(repo)?, json),
                Command::Skill {
                    repo,
                    orchestrating: _, // the default; named only for symmetry with --authoring
                    authoring,
                } => cmd::skill::run(&cmd::repo_root(repo)?, authoring),
                Command::Init { repo } => cmd::init::run(&cmd::repo_root(repo)?),
                Command::Validate { repo, json } => {
                    cmd::validate::run(&cmd::repo_root(repo)?, json)
                }
                Command::Get { task, json } => cmd::get::run(&db, &task, json),
                Command::Trace { task, json } => cmd::trace::run(&db, &task, json),
                Command::BindPane {
                    pane,
                    task,
                    workspace,
                    worktree,
                    branch,
                    base,
                } => cmd::bind_pane::run(&db, &pane, task, workspace, worktree, branch, base),
                Command::Context { task, json } => cmd::context::run(&db, task, json),
                Command::Check { action } => match action {
                    CheckCommand::Submit {
                        pass,
                        fail,
                        author,
                        task,
                    } => cmd::check::submit(&db, pass, fail, author, task),
                },
                Command::Read { id } => cmd::check::read(&db, &id),
                Command::Signal {
                    name,
                    pass,
                    fail,
                    task,
                    author,
                    note,
                } => cmd::signal::run(&db, &name, pass, fail, task, author, note),
                Command::Run { pipeline, task } => cmd::settle::run_pipeline(&db, &pipeline, task),
                Command::Retry { task } => cmd::retry::run(&db, &task),
                Command::Cancel { task, reason } => cmd::cancel::run(&db, &task, reason),
                Command::Forward => cmd::forward::run(&db),
                Command::Raw { limit, json } => cmd::raw::run(&db, limit, json),
                Command::Pause => cmd::pause::run(&db, true),
                Command::Resume => cmd::pause::run(&db, false),
                Command::Supervise { .. } => unreachable!("handled above"),
            }
        }
    }
}
