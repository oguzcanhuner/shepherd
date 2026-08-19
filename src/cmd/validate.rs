use crate::config::{Policy, StepKind, report, resolved_steps};
use anyhow::{Result, bail};
use std::path::Path;

/// `shep validate` — every problem at once, and what the config resolved to when
/// there are none.
pub fn run(repo: &Path, json: bool) -> Result<()> {
    let policy = Policy::read_only(repo)?;
    let problems = policy.problems();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "config": policy.path.to_string_lossy(),
                "repo": policy.repo.to_string_lossy(),
                "valid": problems.is_empty(),
                "problems": problems.iter().map(|p| serde_json::json!({
                    "at": p.at,
                    "message": p.message,
                    "hint": p.hint,
                })).collect::<Vec<_>>(),
            }))?
        );
        if problems.is_empty() {
            return Ok(());
        }
        // Still an error: a script asking whether config is good wants the exit code.
        std::process::exit(1);
    }

    if !problems.is_empty() {
        bail!(report(&policy, &problems));
    }

    println!("{} is valid", policy.path.display());
    for (name, pipeline) in &policy.config.pipeline {
        let mut notes = Vec::new();
        if let Some(on_fail) = &pipeline.on_fail {
            notes.push(format!(
                "on_fail → {on_fail}, max {} round(s)",
                pipeline.max_rounds.unwrap_or(0)
            ));
        }
        if let Some(exhausted) = pipeline.on_exhausted {
            notes.push(format!("on_exhausted {exhausted}"));
        }
        println!(
            "\npipeline {name}{}",
            if notes.is_empty() {
                String::new()
            } else {
                format!("  [{}]", notes.join("; "))
            }
        );
        for (step, kind) in resolved_steps(&policy, name) {
            let awaits = policy
                .step_await(name, &step)
                .map(|s| format!("  ⟳ await {s}"))
                .unwrap_or_default();
            match kind {
                // The filename is the registration, so showing the file is
                // showing the registration.
                Some(StepKind::Script(path)) => {
                    println!("  {step:<14} {}{awaits}", path.display())
                }
                Some(StepKind::Pipeline(inner)) => {
                    println!("  {step:<14} pipeline {inner}{awaits}")
                }
                None => println!("  {step:<14} UNRESOLVED"),
            }
        }
    }
    if !policy.config.signal.is_empty() {
        println!("\nsignals");
        for (sig, decl) in &policy.config.signal {
            println!("  {sig:<14} {}", decl.description);
        }
    }
    for (name, t) in &policy.config.types {
        println!("\ntype {name}  {}", t.description);
        println!("  {}", t.pipelines.join(" → "));
    }
    Ok(())
}
