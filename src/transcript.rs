use serde_json::Value;
use std::fs;

/// Derives a human-friendly session name from a Claude Code transcript, matching
/// what the `/resume` picker shows. Priority: explicit `/rename` > AI-generated
/// title > compaction summary > first user message.
pub fn session_title(transcript_path: &str) -> Option<String> {
    let content = fs::read_to_string(transcript_path).ok()?;
    session_title_from_contents(&content)
}

pub fn session_title_from_contents(content: &str) -> Option<String> {
    let mut renamed: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut first_user_message: Option<String> = None;

    for line in content.split('\n') {
        let Ok(object) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            continue;
        };
        match kind {
            "agent-name" => {
                if let Some(name) = non_empty_str(&object, "agentName") {
                    renamed = Some(name);
                }
            }
            "ai-title" => {
                if let Some(title) = non_empty_str(&object, "aiTitle") {
                    ai_title = Some(title);
                }
            }
            "summary" => {
                if let Some(value) = non_empty_str(&object, "summary") {
                    summary = Some(value);
                }
            }
            "user" if first_user_message.is_none() => {
                if let Some(text) = user_text(&object) {
                    if is_plausible_title(&text) {
                        first_user_message = Some(text);
                    }
                }
            }
            _ => {}
        }
    }

    let raw = renamed.or(ai_title).or(summary).or(first_user_message)?;
    Some(truncate(&raw, 60))
}

fn non_empty_str(object: &Value, key: &str) -> Option<String> {
    match object.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

fn user_text(object: &Value) -> Option<String> {
    let message = object.get("message")?;
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        return Some(text.trim().to_string());
    }
    if let Some(parts) = message.get("content").and_then(Value::as_array) {
        let texts: Vec<&str> = parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect();
        let joined = texts.join(" ").trim().to_string();
        return if joined.is_empty() {
            None
        } else {
            Some(joined)
        };
    }
    None
}

fn is_plausible_title(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('<')
}

fn truncate(text: &str, limit: usize) -> String {
    let flat = text
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let cut: String = flat.chars().take(limit).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_lines(lines: &[serde_json::Value]) -> String {
        lines
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn uses_first_user_message() {
        let content = json_lines(&[serde_json::json!(
            {"type": "user", "message": {"role": "user", "content": "build a dolphin app"}}
        )]);
        assert_eq!(
            session_title_from_contents(&content).as_deref(),
            Some("build a dolphin app")
        );
    }

    #[test]
    fn rename_wins_over_everything() {
        let content = json_lines(&[
            serde_json::json!({"type": "user", "message": {"content": "first message"}}),
            serde_json::json!({"type": "summary", "summary": "A summary"}),
            serde_json::json!({"type": "ai-title", "aiTitle": "AI Title"}),
            serde_json::json!({"type": "agent-name", "agentName": "my-session"}),
        ]);
        assert_eq!(
            session_title_from_contents(&content).as_deref(),
            Some("my-session")
        );
    }

    #[test]
    fn ai_title_wins_over_summary_and_user() {
        let content = json_lines(&[
            serde_json::json!({"type": "user", "message": {"content": "first"}}),
            serde_json::json!({"type": "summary", "summary": "Sum"}),
            serde_json::json!({"type": "ai-title", "aiTitle": "Title"}),
        ]);
        assert_eq!(
            session_title_from_contents(&content).as_deref(),
            Some("Title")
        );
    }

    #[test]
    fn skips_system_reminder_first_message() {
        let content = json_lines(&[
            serde_json::json!({"type": "user", "message": {"content": "<system-reminder>ignore</system-reminder>"}}),
            serde_json::json!({"type": "user", "message": {"content": "the real request"}}),
        ]);
        assert_eq!(
            session_title_from_contents(&content).as_deref(),
            Some("the real request")
        );
    }

    #[test]
    fn joins_array_text_content() {
        let content = json_lines(&[serde_json::json!(
            {"type": "user", "message": {"content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "world"}
            ]}}
        )]);
        assert_eq!(
            session_title_from_contents(&content).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn truncates_long_titles() {
        let long = "a".repeat(80);
        let content = json_lines(&[serde_json::json!(
            {"type": "user", "message": {"content": long}}
        )]);
        let title = session_title_from_contents(&content).unwrap();
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= 61);
    }

    #[test]
    fn returns_none_for_empty() {
        assert_eq!(session_title_from_contents(""), None);
    }
}
