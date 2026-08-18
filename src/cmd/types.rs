use crate::config::Policy;
use anyhow::Result;
use std::path::Path;

/// `shep types` — the menu. This exists so an agent can choose, which is why
/// `description` lives on types and nowhere else.
pub fn run(repo: &Path, json: bool) -> Result<()> {
    let policy = Policy::load(repo)?;

    if json {
        let rows: Vec<_> = policy
            .config
            .types
            .iter()
            .map(|(name, t)| {
                serde_json::json!({
                    "type": name,
                    "description": t.description,
                    "pipelines": t.pipelines,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    let width = policy
        .config
        .types
        .keys()
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(4);
    for (name, t) in &policy.config.types {
        println!("{name:<width$}  {}", t.description, width = width);
        println!("{:<width$}  {}", "", t.pipelines.join(" → "), width = width);
    }
    Ok(())
}
