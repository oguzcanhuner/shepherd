//! One module per subcommand. Each is a thin shell over `engine` and `db`: the
//! CLI holds no logic that the supervisor does not also run.

pub mod create;
pub mod forward;
pub mod pause;
pub mod ps;
pub mod raw;
pub mod status;
pub mod supervise;

/// A duration rendered the way you'd say it out loud. Used by every table.
pub fn ago(seconds: i64) -> String {
    let s = seconds.max(0);
    match s {
        0..=1 => "just now".to_string(),
        2..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// Cut a brief down to a table cell without lying about it.
pub fn truncate(text: &str, width: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= width {
        return one_line;
    }
    let keep: String = one_line.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", keep.trim_end())
}
