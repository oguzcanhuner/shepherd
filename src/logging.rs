//! Logging. Hook and step stdout is not a terminal, so the supervisor logs to a
//! file beside the store and mirrors to stderr only when someone is
//! watching.

use crate::Result;
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

fn filter(default: &str) -> EnvFilter {
    EnvFilter::try_from_env("SHEP_LOG").unwrap_or_else(|_| EnvFilter::new(default))
}

/// For the supervisor: append to `path`, and mirror to stderr if it is a terminal.
pub fn init_file(path: &Path, default_level: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let file = Arc::new(OpenOptions::new().create(true).append(true).open(path)?);
    let builder = tracing_subscriber::fmt().with_env_filter(filter(default_level));

    if std::io::stderr().is_terminal() {
        builder
            .with_writer(file.and(std::io::stderr))
            .with_ansi(false)
            .init();
    } else {
        builder.with_writer(file).with_ansi(false).init();
    }
    Ok(())
}

/// For one-shot CLI commands: stderr only, quiet unless asked.
pub fn init_cli() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter("warn"))
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .try_init();
}
