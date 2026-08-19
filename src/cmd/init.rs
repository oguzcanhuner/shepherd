//! `shep init` — scaffold a `.shep/` directory a task can actually run through.
//!
//! The starter is small and honest: one synchronous check step, one human
//! handoff. It validates out of the box and finishes, so the first task a user
//! creates goes somewhere — and every file says, in its own comments, what to
//! change to make it real.
//!
//! Nothing is ever overwritten. A file that already exists is reported and
//! left alone, so `shep init` in a half-configured repo fills the gaps and
//! touches nothing else.

use anyhow::{Context, Result};
use std::path::Path;

const CONFIG: &str = r#"# Shepherd workflow for this repository.
#
# A type seeds a task's plan: an ordered list of pipelines. A pipeline is an
# ordered list of steps. A step named `check` runs `.shep/scripts/check.sh`,
# which reports its result as one JSON line:
#   {"outcome": "pass" | "reject" | "started" | "error"}
# When a type's plan is spent the task RESTS — idle and non-terminal — until you
# apply another pipeline with `shep run`. That rest is where you take over.
#
# Reference: https://github.com/oguzcanhuner/shepherd/tree/main/docs

[pipeline.check]
steps = ["check"]

[type.task]
description = "Run the checks, then rest for you to look."
pipelines = ["check"]

# Apply a pipeline to a resting task by hand (or have your orchestrator do it):
#
#   shep run integrate --task <id>
#
# A pipeline can retry itself, and a step can defer until a signal resolves it:
#
# [pipeline.review]
# steps        = ["lint", "test"]
# on_fail      = "fix"           # step to run after a rejection
# max_rounds   = 3
# on_exhausted = "reject"
#
# [pipeline.implement]
# steps = [{ run = "implement", await = "agent_stopped", on_missing = "pass" }]
#                                # await: the built-in "agent_stopped", or a
#                                #   [signal.*] you declare and fire with `shep signal`
#                                # on_missing = "pass": a stop with no check counts as a
#                                #   pass; leave unset where a missing verdict is a failure
#
# [signal.ci]
# description = "CI result, fired by `shep signal <task> --name ci --pass|--fail`"
"#;

const CHECK: &str = r#"#!/usr/bin/env bash
# The `check` step. Replace the placeholder with this project's real checks —
# everything printed before the last line is kept as the step's log.
set -euo pipefail

# For example:
#   cargo test 2>&1 || { echo '{"outcome":"reject","note":"tests failed"}'; exit 0; }
#   npm test   2>&1 || { echo '{"outcome":"reject","note":"tests failed"}'; exit 0; }

echo "check.sh is a stub — edit .shep/scripts/check.sh to run real checks"
echo '{"outcome":"pass","note":"stub check"}'
"#;

pub fn run(repo: &Path) -> Result<()> {
    let files = [
        (repo.join(".shep/config.toml"), CONFIG, false),
        (repo.join(".shep/scripts/check.sh"), CHECK, true),
    ];

    for (path, content, executable) in files {
        let shown = path.strip_prefix(repo).unwrap_or(&path).display();
        if path.exists() {
            println!("kept     {shown} (already exists)");
            continue;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
        if executable {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .with_context(|| format!("marking {} executable", path.display()))?;
            }
        }
        println!("created  {shown}");
    }

    // The scaffold has to hold to its own standard: validate what is now there,
    // which also covers the "filled the gaps around existing files" case.
    println!();
    crate::cmd::validate::run(repo, false)?;

    println!();
    println!("Next:");
    println!("  edit .shep/scripts/check.sh        # make the check real");
    println!("  shep create --type task \"try it\"   # queue a first task");
    println!("  shep ps                            # watch it run, then rest");
    Ok(())
}
