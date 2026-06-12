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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let normalized = key.trim();
    match normalized {
        "Enter" | "enter" => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
        "Tab" | "tab" => Some(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty())),
        "Esc" | "esc" => Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
        "Backspace" | "backspace" => Some(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty())),
        "Up" | "up" => Some(KeyEvent::new(KeyCode::Up, KeyModifiers::empty())),
        "Down" | "down" => Some(KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
        "Left" | "left" => Some(KeyEvent::new(KeyCode::Left, KeyModifiers::empty())),
        "Right" | "right" => Some(KeyEvent::new(KeyCode::Right, KeyModifiers::empty())),
        "C-c" | "c-c" | "ctrl+c" => Some(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        _ if normalized.len() == 1 => normalized
            .chars()
            .next()
            .map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())),
        _ => None,
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

/// Cap reported prompts to a sane size and strip control characters that
/// could corrupt the float rendering. Newlines and tabs survive.
pub(super) fn sanitize_reported_prompt(prompt: &str) -> String {
    const MAX_PROMPT_LEN: usize = 16 * 1024;
    // Drop full ANSI CSI/OSC escape sequences, then any leftover controls.
    let mut cleaned = String::with_capacity(prompt.len().min(MAX_PROMPT_LEN + 8));
    let mut chars = prompt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: consume through the final byte (@..~).
                    for follow in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&follow) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: consume through BEL or ESC\.
                    while let Some(follow) = chars.next() {
                        if follow == '\u{7}' {
                            break;
                        }
                        if follow == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if !c.is_control() || c == '\n' || c == '\t' {
            cleaned.push(c);
        }
    }
    if cleaned.len() > MAX_PROMPT_LEN {
        let mut cut = MAX_PROMPT_LEN;
        while !cleaned.is_char_boundary(cut) {
            cut -= 1;
        }
        cleaned.truncate(cut);
        cleaned.push('\u{2026}');
    }
    cleaned.trim().to_string()
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

pub(super) fn normalize_custom_status(status: Option<String>) -> Option<String> {
    let trimmed = status?.trim().to_string();
    let mut normalized = String::new();
    for ch in trimmed.chars().filter(|ch| !ch.is_control()).take(32) {
        normalized.push(ch);
    }
    (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
}
