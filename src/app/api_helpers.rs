pub(super) fn tab_attention_priority(state: crate::detect::AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (crate::detect::AgentState::Blocked, _) => 4,
        (crate::detect::AgentState::Idle, false) => 3,
        (crate::detect::AgentState::Working, _) => 2,
        (crate::detect::AgentState::Idle, true) => 1,
        (crate::detect::AgentState::Unknown, _) => 0,
    }
}

fn parse_api_key(key: &str) -> Option<crossterm::event::KeyEvent> {
    let normalized = normalize_api_key_alias(key.trim());
    let (code, modifiers) = crate::config::parse_key_combo(normalized)?;
    Some(crossterm::event::KeyEvent::new(code, modifiers))
}

fn normalize_api_key_alias(key: &str) -> &str {
    match key {
        "C-c" | "c-c" => "ctrl+c",
        "+" => "plus",
        _ => key,
    }
}

pub(super) fn encode_api_text(runtime: &crate::terminal::TerminalRuntime, text: &str) -> Vec<u8> {
    let bracketed = runtime
        .input_state()
        .map(|state| state.bracketed_paste)
        .unwrap_or(false);
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

pub(super) fn encode_api_keys(
    runtime: &crate::terminal::TerminalRuntime,
    keys: &[String],
) -> Result<Vec<Vec<u8>>, String> {
    let mut encoded_keys = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(key_event) = parse_api_key(key) else {
            return Err(key.clone());
        };
        encoded_keys.push(runtime.encode_terminal_key(key_event.into()));
    }
    Ok(encoded_keys)
}

pub(super) fn encode_api_submission(
    runtime: &crate::terminal::TerminalRuntime,
    text: &str,
) -> Vec<u8> {
    let mut bytes = encode_api_text(runtime, text);
    let enter = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    );
    bytes.extend_from_slice(&runtime.encode_terminal_key(enter.into()));
    bytes
}

pub(super) fn encode_api_input(
    runtime: &crate::terminal::TerminalRuntime,
    text: &str,
    keys: &[String],
) -> Result<Vec<u8>, String> {
    let mut bytes = if text.is_empty() {
        Vec::new()
    } else {
        encode_api_text(runtime, text)
    };
    for encoded in encode_api_keys(runtime, keys)? {
        bytes.extend_from_slice(&encoded);
    }
    Ok(bytes)
}

pub(super) fn detect_state_from_api(
    state: crate::api::schema::PaneAgentState,
) -> crate::detect::AgentState {
    match state {
        crate::api::schema::PaneAgentState::Idle => crate::detect::AgentState::Idle,
        crate::api::schema::PaneAgentState::Working => crate::detect::AgentState::Working,
        crate::api::schema::PaneAgentState::Blocked => crate::detect::AgentState::Blocked,
        crate::api::schema::PaneAgentState::Unknown => crate::detect::AgentState::Unknown,
    }
}

pub(super) fn pane_agent_status(
    state: crate::detect::AgentState,
    seen: bool,
) -> crate::api::schema::AgentStatus {
    match (state, seen) {
        (crate::detect::AgentState::Idle, false) => crate::api::schema::AgentStatus::Done,
        (crate::detect::AgentState::Idle, true) => crate::api::schema::AgentStatus::Idle,
        (crate::detect::AgentState::Working, _) => crate::api::schema::AgentStatus::Working,
        (crate::detect::AgentState::Blocked, _) => crate::api::schema::AgentStatus::Blocked,
        (crate::detect::AgentState::Unknown, _) => crate::api::schema::AgentStatus::Unknown,
    }
}

pub(super) fn read_terminal_snapshot(
    terminal: &crate::terminal::TerminalRuntime,
    source: crate::api::schema::ReadSource,
    format: crate::api::schema::ReadFormat,
    lines: Option<u32>,
) -> String {
    use crate::api::schema::{ReadFormat, ReadSource};

    let requested_lines = lines.unwrap_or(80).min(1000) as usize;
    let text = match (format, source) {
        (ReadFormat::Text, ReadSource::Visible) => terminal.visible_text(),
        (ReadFormat::Text, ReadSource::Recent) => terminal.recent_text(requested_lines),
        (ReadFormat::Text, ReadSource::RecentUnwrapped) => {
            terminal.recent_unwrapped_text(requested_lines)
        }
        (ReadFormat::Text, ReadSource::Detection) => terminal.detection_text(),
        (ReadFormat::Ansi, ReadSource::Visible) => terminal.visible_ansi(),
        (ReadFormat::Ansi, ReadSource::Recent) => terminal.recent_ansi(requested_lines),
        (ReadFormat::Ansi, ReadSource::RecentUnwrapped) => {
            terminal.recent_unwrapped_ansi(requested_lines)
        }
        (ReadFormat::Ansi, ReadSource::Detection) => terminal.detection_text(),
    };

    if lines.is_some() && matches!(source, ReadSource::Visible | ReadSource::Detection) {
        last_lines(&text, requested_lines)
    } else {
        text
    }
}

fn last_lines(text: &str, count: usize) -> String {
    if text.is_empty() || count == 0 {
        return String::new();
    }
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    lines[lines.len().saturating_sub(count)..].concat()
}

#[cfg(test)]
mod read_snapshot_tests {
    use super::last_lines;

    #[test]
    fn last_lines_preserves_real_line_endings_and_unicode() {
        assert_eq!(last_lines("one\ntwø\n三\n", 2), "twø\n三\n");
        assert_eq!(last_lines("one\ntwo\nthree", 1), "three");
        assert_eq!(last_lines("one\ntwo", 0), "");
        assert_eq!(last_lines("", 2), "");
    }
}

pub(super) fn normalize_reported_agent_label(agent: &str) -> Option<String> {
    let trimmed = agent.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(agent) = crate::detect::parse_agent_label(trimmed) {
        return Some(crate::detect::agent_label(agent).to_string());
    }
    Some(trimmed.to_string())
}

pub(super) const METADATA_TTL_MAX_MS: u64 = 86_400_000;
pub(super) const METADATA_SOURCE_MAX_CHARS: usize = 80;
const METADATA_TTL_MIN_MS: u64 = 1;
const MAX_METADATA_TOKEN_KEYS_PER_REQUEST: usize = 16;
pub(super) const MAX_METADATA_TOKEN_KEYS_PER_RESOURCE: usize = 32;
const MAX_METADATA_TOKEN_KEY_LEN: usize = 32;
const MAX_METADATA_TOKEN_VALUE_LEN: usize = 80;

pub(super) fn normalize_metadata_source(value: String) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Err("metadata source must not be empty");
    }
    if value.chars().count() > METADATA_SOURCE_MAX_CHARS {
        return Err("metadata source must be 80 characters or fewer");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
    {
        return Err(
            "metadata source may contain only ASCII letters, digits, colon, dot, underscore, and hyphen",
        );
    }
    Ok(value.to_string())
}

pub(super) fn normalize_metadata_ttl(
    ttl_ms: Option<u64>,
) -> Result<Option<std::time::Duration>, &'static str> {
    let Some(ttl_ms) = ttl_ms else {
        return Ok(None);
    };
    if ttl_ms < METADATA_TTL_MIN_MS {
        return Err("metadata ttl_ms must be at least 1");
    }
    if ttl_ms > METADATA_TTL_MAX_MS {
        return Err("metadata ttl_ms must be 86400000 or less");
    }
    Ok(Some(std::time::Duration::from_millis(ttl_ms)))
}

pub(super) fn normalize_metadata_tokens(
    tokens: std::collections::HashMap<String, Option<String>>,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    if tokens.is_empty() {
        return Err("missing token to set or clear".into());
    }
    if tokens.len() > MAX_METADATA_TOKEN_KEYS_PER_REQUEST {
        return Err(format!(
            "a metadata report may update at most {MAX_METADATA_TOKEN_KEYS_PER_REQUEST} tokens"
        ));
    }

    tokens
        .into_iter()
        .map(|(key, value)| {
            if key.is_empty()
                || key.len() > MAX_METADATA_TOKEN_KEY_LEN
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            {
                return Err(format!("invalid metadata token key: {key}"));
            }
            let value = value.and_then(|value| {
                let normalized = value
                    .trim()
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(MAX_METADATA_TOKEN_VALUE_LEN)
                    .collect::<String>();
                (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
            });
            Ok((key, value))
        })
        .collect()
}

#[cfg(test)]
mod metadata_token_tests {
    use super::*;

    #[test]
    fn token_normalization_sanitizes_values_and_turns_empty_into_clear() {
        let tokens = normalize_metadata_tokens(std::collections::HashMap::from([
            ("summary".into(), Some("  review\nready  ".into())),
            ("empty".into(), Some(" \n ".into())),
            ("clear".into(), None),
        ]))
        .unwrap();

        assert_eq!(tokens["summary"].as_deref(), Some("reviewready"));
        assert_eq!(tokens["empty"], None);
        assert_eq!(tokens["clear"], None);
    }

    #[test]
    fn token_normalization_rejects_invalid_or_unbounded_keys() {
        for key in [
            "bad.name".to_string(),
            "x".repeat(MAX_METADATA_TOKEN_KEY_LEN + 1),
        ] {
            assert!(normalize_metadata_tokens(std::collections::HashMap::from([(
                key,
                Some("value".into()),
            )]))
            .is_err());
        }
        let too_many = (0..=MAX_METADATA_TOKEN_KEYS_PER_REQUEST)
            .map(|index| (format!("key{index}"), Some("value".into())))
            .collect();
        assert!(normalize_metadata_tokens(too_many).is_err());
    }
}
