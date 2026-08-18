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
# A type is an ordered list of pipelines; a pipeline is an ordered list of
# steps. A step named `check` runs `.shep/scripts/check.sh`, which reports its
# result as one JSON line: {"outcome": "pass" | "reject" | "started" | "error"}.
#
# Reference: https://github.com/oguzcanhuner/shepherd/tree/main/docs

[pipeline.check]
steps = ["check"]

[pipeline.handoff]
steps = ["handoff"]
await = "human"          # resolves when you run `shep approve` or `shep reject`

[type.task]
description = "Run the checks, then wait for approval."
pipelines = ["check", "handoff"]

# A pipeline can retry itself, and a deferred pipeline can wait for an agent:
#
# [pipeline.review]
# steps        = ["lint", "test"]
# on_fail      = "fix"           # step to run after a rejection
# max_rounds   = 3
# on_exhausted = "reject"
#
# [pipeline.implement]
# steps   = ["implement"]
# await   = "agent_stopped"      # resolves when the agent in the task's pane stops
# on_stop = "pass"               # stopping with no recorded check counts as a pass;
#                                # leave unset where a missing verdict is a failure
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

const HANDOFF: &str = r#"#!/usr/bin/env bash
# The `handoff` step. Its pipeline has `await = "human"`, so reporting
# "started" hands the task to you: it waits until `shep approve` or
# `shep reject`. Print anything useful for that decision above the last line.
set -euo pipefail

echo "task ${SHEP_TASK_ID} is ready for review"
if [ -n "${SHEP_BRANCH:-}" ] && [ -n "${SHEP_BASE:-}" ]; then
  git diff --stat "${SHEP_BASE}...${SHEP_BRANCH}" || true
fi

echo '{"outcome":"started"}'
"#;

pub fn run(repo: &Path) -> Result<()> {
    let files = [
        (repo.join(".shep/config.toml"), CONFIG, false),
        (repo.join(".shep/scripts/check.sh"), CHECK, true),
        (repo.join(".shep/scripts/handoff.sh"), HANDOFF, true),
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
    println!("  shep ps                            # watch it run");
    Ok(())
}
