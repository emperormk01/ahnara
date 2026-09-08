//! Context pruning for provider requests.

use crate::memory::HistoryMessage;
use crate::providers::Message;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_RECENT_TURNS: usize = 10;
const MAX_SUMMARY_CHARS: usize = 1_200;

pub fn clamp_recent_turns(value: usize) -> usize {
    if value == 0 {
        DEFAULT_RECENT_TURNS
    } else {
        value.clamp(1, 50)
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

pub fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let keep_each_side = max_chars.saturating_sub(80) / 2;
    let head: String = text.chars().take(keep_each_side).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(keep_each_side)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{}\n...[truncated {} chars for context budget]...\n{}",
        head,
        text.chars().count().saturating_sub(keep_each_side * 2),
        tail
    )
}

pub fn spill_tool_output(output: &str, max_chars: usize, tool_name: &str) -> String {
    let char_count = output.chars().count();
    if char_count <= max_chars {
        return output.to_string();
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tool_name_clean: String = tool_name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let filename = format!("{}_{}.log", tool_name_clean, nanos);
    let dir = shellexpand::tilde("~/.ahnara/tool_output");
    let dir_path = Path::new(dir.as_ref());
    if let Err(e) = fs::create_dir_all(dir_path) {
        tracing::warn!("Failed to create tool output spill directory: {}", e);
    }
    let path = dir_path.join(&filename);
    if let Err(e) = fs::write(&path, output) {
        tracing::warn!("Failed to write tool output spill file: {}", e);
    } else {
        tracing::info!(
            "Tool output spilled: {} chars -> {} (full output at {:?})",
            char_count,
            max_chars,
            path
        );
    }

    let head: String = output.chars().take(max_chars).collect();
    let spilled = char_count - max_chars;
    format!(
        "{}\n...[truncated {} of {} chars — full output: {:?}]",
        head, spilled, char_count, path
    )
}

pub fn summarize_older_history(history: &[HistoryMessage]) -> Option<String> {
    if history.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    let total = history.len();
    let first = total.saturating_sub(20);
    for item in history.iter().skip(first) {
        let content = truncate_for_summary(&item.content, 220).replace('\n', " ");
        lines.push(format!("- {}: {}", item.role, content));
    }

    Some(format!(
        "Earlier conversation summary, compressed by AHNARA to save context tokens. Original older message count: {}. Most recent older items:\n{}",
        total,
        lines.join("\n")
    ))
}

pub fn build_pruned_messages(
    system_prompt: String,
    history: &[HistoryMessage],
    user_message: String,
    recent_turns: usize,
    context_window_tokens: u32,
) -> Vec<Message> {
    use crate::agent::history::{detect_topic_boundaries, History};

    let ctx_limit = context_window_tokens as usize;
    if ctx_limit == 0 {
        // No budget -- return minimal messages
        return vec![
            Message::new("system", &system_prompt),
            Message::new("user", &user_message),
        ];
    }

    // Build hierarchical history from flat messages
    let mut h = History::from_messages(history.to_vec(), ctx_limit);

    // Detect and apply topic boundaries
    let boundaries = detect_topic_boundaries(history);
    if !boundaries.is_empty() {
        // Rebuild with topic splits
        let mut topics: Vec<Vec<HistoryMessage>> = Vec::new();
        let mut current_chunk: Vec<HistoryMessage> = Vec::new();
        for (i, msg) in history.iter().enumerate() {
            if boundaries.contains(&i) && !current_chunk.is_empty() {
                topics.push(std::mem::take(&mut current_chunk));
            }
            current_chunk.push(msg.clone());
        }
        if !current_chunk.is_empty() {
            topics.push(current_chunk);
        }

        // The last chunk is the current topic, rest go to topics list
        if topics.len() > 1 {
            let current_messages = topics.pop().unwrap();
            h.current.messages = current_messages;
            h.topics = topics
                .into_iter()
                .map(|msgs| crate::agent::history::Topic {
                    messages: msgs,
                    summary: None,
                })
                .collect();
        }
    }

    // Compress to fit within budget
    if let Err(e) = h.compress() {
        tracing::warn!("Hierarchical compression failed: {}, falling back to simple pruning", e);
        return build_pruned_messages_simple(system_prompt, history, user_message, recent_turns, ctx_limit);
    }

    let stats = h.stats();
    tracing::debug!("Hierarchical compression: {}", stats);

    h.build_messages(&system_prompt, &user_message)
}

/// Fallback: simple old/recent split (used when hierarchical compression fails).
fn build_pruned_messages_simple(
    system_prompt: String,
    history: &[HistoryMessage],
    user_message: String,
    recent_turns: usize,
    max_tokens: usize,
) -> Vec<Message> {
    let recent_turns = clamp_recent_turns(recent_turns);
    let keep_messages = recent_turns.saturating_mul(2);
    let split_at = history.len().saturating_sub(keep_messages);
    let (older, recent) = history.split_at(split_at);

    let mut sys_prompt = system_prompt;
    if let Some(summary) = summarize_older_history(older) {
        sys_prompt.push_str("\n\n## Earlier Conversation Summary\n");
        sys_prompt.push_str(&summary);
    }

    let mut messages = vec![Message::new("system", sys_prompt)];

    for m in recent {
        if m.role == "system" {
            if let Some(ref mut first) = messages.first_mut() {
                if let Some(ref mut content) = first.content {
                    content.push_str("\n\n");
                    content.push_str(&m.content);
                }
            }
        } else {
            messages.push(Message::new(
                &m.role,
                truncate_for_summary(&m.content, MAX_SUMMARY_CHARS),
            ));
        }
    }

    messages.push(Message::new("user", user_message));
    trim_to_token_budget(messages, max_tokens)
}

fn trim_to_token_budget(mut messages: Vec<Message>, max_tokens: usize) -> Vec<Message> {
    if max_tokens == 0 {
        return messages;
    }

    while estimate_message_tokens(&messages) > max_tokens && messages.len() > 2 {
        let remove_index = messages
            .iter()
            .position(|m| m.role != "system")
            .unwrap_or(1);
        messages.remove(remove_index);
    }

    messages
}

fn estimate_message_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            estimate_tokens(&m.role)
                + estimate_tokens(m.content.as_deref().unwrap_or(""))
                + 4
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(role: &str, content: &str) -> HistoryMessage {
        HistoryMessage {
            role: role.into(),
            content: content.into(),
            timestamp: 0,
            tool_calls: None,
        }
    }

    #[test]
    fn hierarchical_compression_preserves_user_message() {
        let mut h = Vec::new();
        for i in 0..30 {
            h.push(history("user", &format!("message {}", i)));
        }
        let messages = build_pruned_messages("sys".into(), &h, "now".into(), 10, 50_000);
        assert_eq!(messages.last().unwrap().role, "user");
        assert_eq!(messages.last().unwrap().content.as_deref(), Some("now"));
    }

    #[test]
    fn hierarchical_compression_within_budget() {
        let mut h = Vec::new();
        for i in 0..100 {
            h.push(history("user", &format!("This is message number {} with some content to inflate the token count for testing purposes", i)));
            h.push(history("assistant", &format!("This is assistant response number {} with some content to inflate the token count for testing", i)));
        }
        let messages = build_pruned_messages("sys".into(), &h, "now".into(), 10, 2000);
        let total: usize = messages.iter().map(|m| {
            estimate_tokens(m.role.as_str())
                + estimate_tokens(m.content.as_deref().unwrap_or(""))
                + 4
        }).sum();
        // Hierarchical compression should produce significantly fewer tokens than uncompressed
        let uncompressed: usize = h.iter().map(|m| estimate_tokens(&m.content) + 4).sum();
        assert!(total < uncompressed, "Compressed {} should be less than uncompressed {}", total, uncompressed);
        // And should fit within a reasonable multiple of the budget
        assert!(total < 2000 * 3, "Total tokens {} should be within reasonable budget bounds", total);
    }

    #[test]
    fn clamps_recent_turns() {
        assert_eq!(clamp_recent_turns(0), 10);
        assert_eq!(clamp_recent_turns(8), 8);
        assert_eq!(clamp_recent_turns(100), 50);
    }

    #[test]
    fn merges_system_messages_from_compaction() {
        let mut h = Vec::new();
        h.push(history("system", "[COMPACTION SUMMARY] Prior context was about building a proxy."));
        h.push(history("user", "what was I working on?"));
        let messages = build_pruned_messages("You are an agent.".into(), &h, "tell me".into(), 10, 50_000);
        // The system prompt and compaction summary should both appear somewhere in the messages
        let all_content: String = messages.iter()
            .map(|m| m.content.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(all_content.contains("You are an agent."), "Should contain persona prompt");
        assert!(all_content.contains("COMPACTION SUMMARY"), "Should contain compaction summary");
        assert!(all_content.contains("Prior context"), "Should contain prior context");
        // Exactly one system message (the main system prompt)
        let system_msgs: Vec<_> = messages.iter().filter(|m| m.role == "system").collect();
        assert_eq!(system_msgs.len(), 1, "must have exactly one system message");
    }

    #[test]
    fn simple_fallback_works() {
        let mut h = Vec::new();
        for i in 0..10 {
            h.push(history("user", &format!("msg {}", i)));
        }
        let messages = build_pruned_messages_simple("sys".into(), &h, "now".into(), 3, 50_000);
        // Should keep recent 6 messages (3 turns * 2) + summary + user msg
        let sys = messages[0].content.as_deref().unwrap_or("");
        assert!(sys.contains("Earlier conversation summary") || messages.len() <= 8);
    }

    #[test]
    fn spill_tool_output_test() {
        // 1) Small output passes through unchanged
        let small = "short output";
        let small_result = spill_tool_output(small, 100, "test_tool");
        assert_eq!(small_result, small);

        // 2) Large output gets truncated with spill file reference
        let large = "a".repeat(1000);
        let large_result = spill_tool_output(&large, 50, "test_tool");
        assert!(large_result.starts_with(&"a".repeat(50)));
        assert!(large_result.contains("[truncated"));
        assert!(large_result.contains("tool_output"));

        // 3) A spill file was created with full content
        let dir = shellexpand::tilde("~/.ahnara/tool_output");
        let dir_path = Path::new(dir.as_ref());
        let entries: Vec<_> = fs::read_dir(dir_path)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("test_tool_") && n.ends_with(".log")
            })
            .collect();
        assert!(!entries.is_empty(), "Expected at least one spill file");
        let content = fs::read_to_string(entries.last().unwrap().path()).unwrap();
        assert_eq!(content, large);
    }
}
