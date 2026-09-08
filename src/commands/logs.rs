//! Logs command - view gateway logs across all channels

use std::path::PathBuf;

const DEFAULT_LOG_DIR: &str = "~/.ahnara/logs";
const DEFAULT_LINES: usize = 50;

/// Handle the /logs command.
///
/// Usage: /logs [lines] [--filter PATTERN] [--errors] [--tail N]
///
/// Subcommands:
///   /logs               - Show last 50 lines
///   /logs 100           - Show last 100 lines
///   /logs --errors      - Show only error/warn lines
///   /logs --filter X    - Show lines matching pattern
///   /logs --clear       - Truncate the log file
pub async fn handle_logs(args: &str) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let mut lines = DEFAULT_LINES;
    let mut filter: Option<String> = None;
    let mut errors_only = false;
    let mut clear = false;

    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "--filter" | "-f" => {
                if i + 1 < parts.len() {
                    filter = Some(parts[i + 1].to_string());
                    i += 2;
                } else {
                    return "Error: --filter requires a pattern".to_string();
                }
            }
            "--errors" | "-e" => {
                errors_only = true;
                i += 1;
            }
            "--clear" | "-c" => {
                clear = true;
                i += 1;
            }
            _ => {
                if let Ok(n) = parts[i].parse::<usize>() {
                    lines = n;
                }
                i += 1;
            }
        }
    }

    let log_path = resolve_log_path();

    if clear {
        return clear_logs(&log_path);
    }

    read_logs(&log_path, lines, filter.as_deref(), errors_only)
}

fn resolve_log_path() -> PathBuf {
    let expanded = shellexpand::tilde(DEFAULT_LOG_DIR);
    PathBuf::from(expanded.as_ref()).join("gateway.log")
}

fn read_logs(path: &PathBuf, max_lines: usize, filter: Option<&str>, errors_only: bool) -> String {
    if !path.exists() {
        return format!(
            "No log file found at {}\n\nLogs are written when the gateway runs with file logging enabled.",
            path.display()
        );
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("Error reading log file: {}", e),
    };

    let all_lines: Vec<&str> = content.lines().collect();

    let filtered: Vec<&str> = all_lines
        .iter()
        .filter(|line| {
            if errors_only {
                let lower = line.to_lowercase();
                lower.contains("error") || lower.contains("warn") || lower.contains("failed")
            } else if let Some(pat) = filter {
                line.contains(pat)
            } else {
                true
            }
        })
        .copied()
        .collect();

    let total_filtered = filtered.len();
    let start = if total_filtered > max_lines {
        total_filtered - max_lines
    } else {
        0
    };

    let visible = &filtered[start..];

    if visible.is_empty() {
        return "Log file is empty (no matching lines).".to_string();
    }

    let mut output = format!(
        "Gateway logs ({} of {} lines)\n\n",
        visible.len(),
        total_filtered
    );

    for line in visible {
        output.push_str(line);
        output.push('\n');
    }

    // Truncate if still too long for channel display (e.g. Telegram 4096 char limit)
    if output.len() > 3800 {
        let truncated: String = output.chars().take(3800).collect();
        format!("{}...\n\n[output truncated - use /logs --filter to narrow]", truncated)
    } else {
        output
    }
}

fn clear_logs(path: &PathBuf) -> String {
    if !path.exists() {
        return "No log file to clear.".to_string();
    }

    match std::fs::write(path, "") {
        Ok(_) => "Log file cleared.".to_string(),
        Err(e) => format!("Error clearing log file: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_log_path_expands_tilde() {
        let path = resolve_log_path();
        let path_str = path.to_string_lossy();
        assert!(!path_str.contains('~'));
        assert!(path_str.ends_with("gateway.log"));
    }

    #[test]
    fn read_logs_returns_message_when_missing() {
        let path = PathBuf::from("/tmp/nonexistent-ahnara-test.log");
        let result = read_logs(&path, 50, None, false);
        assert!(result.contains("No log file found"));
    }
}
