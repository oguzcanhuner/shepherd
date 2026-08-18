use crate::db::{self, raw_event};
use anyhow::{Result, bail};
use std::io::Read;
use std::path::Path;

/// The event JSON Herdr puts in a hook's environment. stdin is empty for hooks,
/// so this is the whole input (herdr-findings §4.4).
pub const EVENT_JSON: &str = "HERDR_PLUGIN_EVENT_JSON";
/// The dotted event name. Note Herdr underscores the same name inside the JSON.
pub const EVENT_NAME: &str = "HERDR_PLUGIN_EVENT";

/// `shep forward` — what `hooks/forward.sh` runs. Appends the raw event and
/// exits; it decides nothing, because the payload has no previous-status field
/// and a hook that kept state of its own would be a second source of truth.
pub fn run(db_path: &Path) -> Result<()> {
    let body = match std::env::var(EVENT_JSON) {
        Ok(json) if !json.trim().is_empty() => json,
        // Not set: either this was run by hand or Herdr changed the contract.
        // Reading stdin makes both testable and pipeable.
        _ => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            if buf.trim().is_empty() {
                bail!(
                    "nothing to forward: ${EVENT_JSON} is unset and stdin is empty. \
                     This runs as a Herdr event hook."
                );
            }
            buf
        }
    };

    let conn = db::open(db_path)?;
    let seq = raw_event::append(&conn, body.trim())?;

    // Hook stdout lands in `herdr plugin log list`, which is where you look when
    // a hook is not firing.
    let name = std::env::var(EVENT_NAME).unwrap_or_else(|_| "?".to_string());
    println!("forwarded {name} as raw_event {seq}");
    Ok(())
}
