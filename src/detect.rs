//! Agent state detection via terminal tail pattern matching.
//!
//! Each pane's live bottom-of-buffer text is read periodically and matched
//! against known agent output patterns to determine state.

/// The detected state of a terminal pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Agent finished, prompt visible, nothing happening.
    Idle,
    /// Agent is actively working/processing.
    Working,
    /// Agent needs human input and is blocked on a response.
    Blocked,
    /// Plain shell or unrecognized program.
    Unknown,
}

/// Which agent we detected running in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Pi,
    Claude,
    Codex,
    Gemini,
    Cursor,
    Cline,
    OpenCode,
    GithubCopilot,
    Kimi,
    Droid,
    Amp,
    Hermes,
}

pub fn agent_label(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "pi",
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Gemini => "gemini",
        Agent::Cursor => "cursor",
        Agent::Cline => "cline",
        Agent::OpenCode => "opencode",
        Agent::GithubCopilot => "copilot",
        Agent::Kimi => "kimi",
        Agent::Droid => "droid",
        Agent::Amp => "amp",
        Agent::Hermes => "hermes",
    }
}

pub fn parse_agent_label(agent: &str) -> Option<Agent> {
    let name = agent.trim().to_lowercase();
    match name.as_str() {
        "pi" => Some(Agent::Pi),
        "claude" | "claude-code" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        "gemini" => Some(Agent::Gemini),
        "cursor" => Some(Agent::Cursor),
        "cline" => Some(Agent::Cline),
        "opencode" | "open-code" => Some(Agent::OpenCode),
        "copilot" | "github-copilot" | "ghcs" => Some(Agent::GithubCopilot),
        "kimi" => Some(Agent::Kimi),
        "droid" => Some(Agent::Droid),
        "amp" | "amp-local" => Some(Agent::Amp),
        "hermes" => Some(Agent::Hermes),
        _ => None,
    }
}

/// Identify which agent is running from the process name.
/// Returns `None` for plain shells or unrecognized programs.
pub fn identify_agent(process_name: &str) -> Option<Agent> {
    let name = process_name.to_lowercase();
    // Match against known binary names
    match name.as_str() {
        "pi" => Some(Agent::Pi),
        "claude" | "claude-code" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        "gemini" => Some(Agent::Gemini),
        "cursor" => Some(Agent::Cursor),
        "cline" => Some(Agent::Cline),
        "opencode" | "open-code" => Some(Agent::OpenCode),
        "github-copilot" | "ghcs" => Some(Agent::GithubCopilot),
        "kimi" => Some(Agent::Kimi),
        "droid" => Some(Agent::Droid),
        "amp" | "amp-local" => Some(Agent::Amp),
        "hermes" => Some(Agent::Hermes),
        _ => None,
    }
}

pub fn identify_agent_in_job(job: &crate::platform::ForegroundJob) -> Option<(Agent, String)> {
    let mut best: Option<(u8, Agent, String)> = None;

    for process in &job.processes {
        let candidate = normalized_process_name(process);
        let Some(agent) = identify_agent(&candidate) else {
            continue;
        };
        let score = process_priority(process, &candidate);

        match &best {
            Some((best_score, _, _)) if *best_score >= score => {}
            _ => best = Some((score, agent, candidate)),
        }
    }

    best.map(|(_, agent, name)| (agent, name))
}

/// Detect the state of an agent from the live terminal tail snapshot.
/// If `agent` is `None`, returns `Unknown`.
pub fn detect_state(agent: Option<Agent>, screen_content: &str) -> AgentState {
    let Some(agent) = agent else {
        return AgentState::Unknown;
    };
    match agent {
        Agent::Pi => detect_pi(screen_content),
        Agent::Claude => detect_claude(screen_content),
        Agent::Codex => detect_codex(screen_content),
        Agent::Gemini => detect_gemini(screen_content),
        Agent::Cursor => detect_cursor(screen_content),
        Agent::Cline => detect_cline(screen_content),
        Agent::OpenCode => detect_opencode(screen_content),
        Agent::GithubCopilot => detect_github_copilot(screen_content),
        Agent::Kimi => detect_kimi(screen_content),
        Agent::Droid => detect_droid(screen_content),
        Agent::Amp => detect_amp(screen_content),
        Agent::Hermes => detect_hermes(screen_content),
    }
}

// ---------------------------------------------------------------------------
// Per-agent detectors
// ---------------------------------------------------------------------------

fn detect_pi(content: &str) -> AgentState {
    // pi shows "Working..." when the agent is processing
    if content.contains("Working...") {
        return AgentState::Working;
    }
    AgentState::Idle
}

/// Claude Code detection. The most complex — it has a structured prompt box UI.
///
/// Screen layout:
/// ```text
///   (agent output / tool results)
///   ───────────────────────── (top border)
///   ❯ _                      (prompt line)
///   ───────────────────────── (bottom border)
/// ```
fn detect_claude(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Search prompt is always idle
    if content.contains("⌕ Search…") {
        return AgentState::Idle;
    }

    // ctrl+r toggle — don't change state
    // (we return Idle as a safe default since we don't have previous state here)
    if lower.contains("ctrl+r to toggle") {
        return AgentState::Idle;
    }

    // --- Blocked detection (full content including prompt box) ---

    if has_claude_blocked_prompt(content, &lower) {
        return AgentState::Blocked;
    }

    // --- Working detection (content above the prompt box) ---

    let above = content_above_prompt_box(content);
    let above_lower = above.to_lowercase();

    if above_lower.contains("esc to interrupt") || above_lower.contains("ctrl+c to interrupt") {
        return AgentState::Working;
    }

    if has_spinner_activity(above) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_codex(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked patterns
    if lower.contains("press enter to confirm or esc to cancel")
        || lower.contains("enter to submit answer")
        || lower.contains("allow command?")
        || lower.contains("[y/n]")
        || lower.contains("yes (y)")
    {
        return AgentState::Blocked;
    }
    if has_confirmation_prompt(&lower) {
        return AgentState::Blocked;
    }

    // Working
    if has_interrupt_pattern(&lower) || has_codex_working_header(content) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_gemini(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked — explicit confirmation
    if lower.contains("waiting for user confirmation") {
        return AgentState::Blocked;
    }

    // Blocked — box-drawing confirmation prompts
    if content.contains("│ Apply this change")
        || content.contains("│ Allow execution")
        || content.contains("│ Do you want to proceed")
    {
        return AgentState::Blocked;
    }
    if has_confirmation_prompt(&lower) {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("esc to cancel") {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_cursor(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if lower.contains("(y) (enter)")
        || lower.contains("keep (n)")
        || lower.contains("skip (esc or n)")
    {
        return AgentState::Blocked;
    }
    // "allow ...(y)" or "run ...(y)" patterns
    if lower.contains("(y)") && (lower.contains("allow") || lower.contains("run")) {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("ctrl+c to stop") {
        return AgentState::Working;
    }
    if has_cursor_spinner(content) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_cline(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if lower.contains("let cline use this tool") {
        return AgentState::Blocked;
    }
    // [act mode] or [plan mode] followed by "yes"
    if (lower.contains("[act mode]") || lower.contains("[plan mode]")) && lower.contains("yes") {
        return AgentState::Blocked;
    }

    // Idle
    if lower.contains("cline is ready for your message") {
        return AgentState::Idle;
    }

    // Cline defaults to working (unlike most agents that default to idle)
    AgentState::Working
}

fn detect_opencode(content: &str) -> AgentState {
    // Blocked
    if content.contains("△ Permission required") {
        return AgentState::Blocked;
    }

    // Working
    if has_interrupt_pattern(&content.to_lowercase()) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_github_copilot(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if lower.contains("│ do you want") {
        return AgentState::Blocked;
    }
    if lower.contains("confirm with") && lower.contains("enter") {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("esc to cancel") {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_kimi(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if lower.contains("allow?")
        || lower.contains("confirm?")
        || lower.contains("approve?")
        || lower.contains("proceed?")
        || lower.contains("[y/n]")
        || lower.contains("(y/n)")
    {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("thinking")
        || lower.contains("processing")
        || lower.contains("generating")
        || lower.contains("waiting for response")
        || lower.contains("ctrl+c to cancel")
        || lower.contains("ctrl-c to cancel")
    {
        return AgentState::Working;
    }

    AgentState::Idle
}

/// Droid detection.
///
/// Working: braille spinner line (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) + "Thinking..." + "(Press ESC to stop)"
/// Blocked: EXECUTE prompt with selection box ("Yes, allow" / "No, cancel") +
///          "Use ↑↓ to navigate, Enter to select"
/// Idle: prompt box visible, no spinner, no selection prompt
fn detect_droid(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    if is_droid_mission_control(content, &lower) {
        return AgentState::Idle;
    }

    // Blocked: EXECUTE approval prompt with selection UI chrome
    // Primary (AND): structural keyword + chrome text = certain
    let has_execute = content.contains("EXECUTE");
    let has_selection_chrome = lower.contains("enter to select")
        || lower.contains("↑↓ to navigate")
        || lower.contains("esc to cancel");
    let has_selection_options = lower.contains("> yes, allow") || lower.contains("> no, cancel");

    if has_execute && (has_selection_chrome || has_selection_options) {
        return AgentState::Blocked;
    }
    // Secondary: selection chrome + options together (no EXECUTE needed)
    if has_selection_chrome && has_selection_options {
        return AgentState::Blocked;
    }

    // Working: braille spinner character at start of a line + "Thinking..."
    // The braille chars (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) are very specific — won't appear in normal content
    if has_braille_spinner(content) && lower.contains("esc to stop") {
        return AgentState::Working;
    }
    // Fallback: "ESC to stop" alone is still a strong signal (it's UI chrome)
    if lower.contains("esc to stop") {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn is_droid_mission_control(content: &str, lower_content: &str) -> bool {
    let has_header = lower_content.contains("mission control");
    let has_active_feature = lower_content.contains("active feature");
    let has_feature_summary = lower_content.contains("features ");
    let has_mission_metrics = lower_content.contains("time ")
        && lower_content.contains("input ")
        && lower_content.contains("output ");

    content.contains("⛬")
        && has_header
        && has_active_feature
        && (has_feature_summary || has_mission_metrics)
}

/// Amp (Sourcegraph) detection.
///
/// Blocked approval prompts use a shared footer with options like
/// "Approve", "Allow All for This Session", "Allow All for Every Session",
/// "Allow File for Every Session", and "Deny with feedback". The header varies
/// by approval type, for example "Invoke tool ...?", "Run this command?",
/// "Allow editing file:", or "Allow creating file:".
///
/// Working layout:
/// ```text
///   ✓ Search Map the core runtime architecture...
///   ⋯ Oracle ▼
///   ≈ Running tools...         Esc to cancel
/// ```
fn detect_amp(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    let has_waiting_for_approval = lower.contains("waiting for approval");
    let has_approval_header = lower.contains("invoke tool")
        || lower.contains("run this command?")
        || lower.contains("allow editing file:")
        || lower.contains("allow creating file:")
        || lower.contains("confirm tool call");
    let has_approval_actions = lower.contains("approve")
        && (lower.contains("allow all for this session")
            || lower.contains("allow all for every session")
            || lower.contains("allow file for every session")
            || lower.contains("deny with feedback"));

    if has_approval_actions && (has_waiting_for_approval || has_approval_header) {
        return AgentState::Blocked;
    }

    if lower.contains("esc to cancel") {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn detect_hermes(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    if lower.contains("resumed session") || lower.contains("previous conversation") {
        return AgentState::Idle;
    }

    if has_hermes_thinking_spinner(content) || has_hermes_tool_spinner(content) {
        return AgentState::Working;
    }

    if has_standard_prompt_box(content) {
        return AgentState::Idle;
    }

    if has_hermes_blocked_prompt(content) {
        return AgentState::Blocked;
    }

    AgentState::Unknown
}

/// Check for braille spinner characters at the start of a line.
/// These are the Unicode braille pattern dots used by CLI spinners.
fn has_braille_spinner(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(c) = trimmed.chars().next() {
            if ('\u{2800}'..='\u{28FF}').contains(&c) {
                return true;
            }
        }
    }
    false
}

fn has_hermes_thinking_spinner(content: &str) -> bool {
    const THINKING_VERBS: &[&str] = &[
        "pondering...",
        "contemplating...",
        "musing...",
        "cogitating...",
        "ruminating...",
        "deliberating...",
        "mulling...",
        "reflecting...",
        "processing...",
        "reasoning...",
        "analyzing...",
        "computing...",
        "synthesizing...",
        "formulating...",
        "brainstorming...",
    ];
    hermes_recent_lines(content).iter().any(|line| {
        let trimmed = line.trim().to_lowercase();
        THINKING_VERBS.iter().any(|verb| trimmed.ends_with(verb))
    })
}

fn has_hermes_tool_spinner(content: &str) -> bool {
    hermes_recent_lines(content).iter().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with('🖥')
            || trimmed.starts_with('💻')
            || trimmed.starts_with('📄')
            || trimmed.starts_with('✏')
            || trimmed.starts_with('🔎')
            || trimmed.starts_with('🌐')
            || trimmed.starts_with('🧠')
            || trimmed.starts_with('🔀')
            || trimmed.starts_with('📁')
            || trimmed.starts_with('⚙')
            || trimmed.starts_with('⚡')
            || trimmed.starts_with('📚')
            || trimmed.starts_with('🐍')
    })
}

fn has_hermes_blocked_prompt(content: &str) -> bool {
    let recent = hermes_recent_lines(content)
        .into_iter()
        .map(|line| line.trim().to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    recent.contains("dangerous command")
        || recent.contains("allow once")
        || recent.contains("allow for this session")
        || recent.contains("add to permanent allowlist")
        || recent.contains("hermes needs your input")
        || recent.contains("type your answer and press enter")
        || recent.contains("↑/↓ to select, enter to confirm")
        || recent.contains("sudo password required")
        || recent.contains("enter password below")
}

fn hermes_recent_lines(content: &str) -> Vec<&str> {
    let lines: Vec<&str> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(6);
    lines[start..].to_vec()
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Check for "do you want"/"would you like" followed by "yes" or "❯"
fn has_confirmation_prompt(lower_content: &str) -> bool {
    if let Some(pos) = lower_content
        .find("do you want")
        .or_else(|| lower_content.find("would you like"))
    {
        let after = &lower_content[pos..];
        return after.contains("yes") || after.contains('❯');
    }
    false
}

/// Claude uses the same generic Select and Dialog widgets for both
/// permission flows and ordinary slash/settings menus. Match only the
/// permission and interview prompts that actually need user input.
fn has_claude_blocked_prompt(content: &str, lower_content: &str) -> bool {
    has_confirmation_prompt(lower_content)
        || lower_content.contains("do you want to proceed?")
        || lower_content.contains("would you like to proceed?")
        || lower_content.contains("waiting for permission")
        || lower_content.contains("do you want to allow this connection?")
        || lower_content.contains("tab to amend")
        || lower_content.contains("ctrl+e to explain")
        || lower_content.contains("chat about this")
        || lower_content.contains("review your answers")
        || lower_content.contains("skip interview and plan immediately")
        || (has_selection_prompt(content) && has_claude_yes_no_choice(content))
}

fn has_claude_yes_no_choice(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line
            .trim()
            .trim_start_matches('❯')
            .trim_start()
            .to_lowercase();
        trimmed == "yes"
            || trimmed == "no"
            || trimmed.starts_with("1. yes")
            || trimmed.starts_with("2. no")
            || trimmed.starts_with("yes, and ")
            || trimmed.starts_with("no, and tell claude")
    })
}

/// Check for "❯" followed by numbered options like "1."
fn has_selection_prompt(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('❯') {
            // Check if there's a digit followed by a dot nearby
            if trimmed.chars().any(|c| c.is_ascii_digit()) && trimmed.contains('.') {
                return true;
            }
        }
    }
    false
}

/// Check for "esc" + "interrupt" pattern
fn has_interrupt_pattern(lower_content: &str) -> bool {
    lower_content.contains("esc to interrupt")
        || lower_content.contains("ctrl+c to interrupt")
        || (lower_content.contains("esc") && lower_content.contains("interrupt"))
}

/// Claude Code spinner characters + activity label.
/// The verb changes frequently ("Processing…", "Pouncing…", etc.), so rely
/// on the spinner glyph + trailing ellipsis rather than specific wording.
/// Include Claude's narrow-pane middle-dot frame too.
fn has_spinner_activity(content: &str) -> bool {
    const SPINNER_CHARS: &str = "·✱✲✳✴✵✶✷✸✹✺✻✼✽✾✿❀❁❂❃❇❈❉❊❋✢✣✤✥✦✧✨⊛⊕⊙◉◎◍⁂⁕※⍟☼★☆";
    for line in content.lines() {
        let trimmed = line.trim();
        let mut chars = trimmed.chars();
        if let Some(first) = chars.next() {
            if SPINNER_CHARS.contains(first) {
                let rest: String = chars.collect();
                if rest.starts_with(' ')
                    && rest.contains('\u{2026}')
                    && rest.chars().any(|c| c.is_alphanumeric())
                {
                    return true;
                }
            }
        }
    }
    false
}

fn has_codex_working_header(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with('•') && trimmed.contains("Working (")
    })
}

/// Cursor spinner: ⬡ or ⬢ followed by a word ending in "ing"
fn has_cursor_spinner(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if (trimmed.starts_with('⬡') || trimmed.starts_with('⬢')) && trimmed.contains("ing") {
            return true;
        }
    }
    false
}

/// Extract content above Claude's prompt box.
/// The prompt box is two ─── border lines with ❯ between them.
fn content_above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let mut border_count = 0;

    for i in (0..lines.len()).rev() {
        let trimmed = lines[i].trim();
        if !trimmed.is_empty() && trimmed.chars().all(|c| c == '─') {
            border_count += 1;
            if border_count == 2 {
                // Return everything above this border
                let byte_offset: usize = lines[..i].iter().map(|l| l.len() + 1).sum();
                return &content[..byte_offset.min(content.len())];
            }
        }
    }

    // No prompt box found, return all content
    content
}

fn has_standard_prompt_box(content: &str) -> bool {
    !std::ptr::eq(content_above_prompt_box(content), content)
}

// ---------------------------------------------------------------------------
// Process identification (platform-specific)
// ---------------------------------------------------------------------------

/// Get the foreground job for a given child PID.
/// Delegates to platform-specific implementation.
pub fn foreground_job(child_pid: u32) -> Option<crate::platform::ForegroundJob> {
    crate::platform::foreground_job(child_pid)
}

fn normalized_process_name(process: &crate::platform::ForegroundProcess) -> String {
    let effective = process.argv0.as_deref().unwrap_or(&process.name);
    let lower_effective = effective.to_lowercase();
    let lower_cmdline = process
        .cmdline
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();

    if lower_effective == "node"
        && (lower_cmdline.contains("/codex") || lower_cmdline.contains("@openai/codex"))
    {
        return "codex".to_string();
    }

    effective.to_string()
}

fn process_priority(process: &crate::platform::ForegroundProcess, normalized_name: &str) -> u8 {
    let lower_name = normalized_name.to_lowercase();
    if lower_name != process.name.to_lowercase() {
        return 3;
    }
    if !is_generic_runtime_or_shell(&lower_name) {
        return 2;
    }
    1
}

fn is_generic_runtime_or_shell(name: &str) -> bool {
    matches!(
        name,
        "sh" | "bash" | "zsh" | "fish" | "tmux" | "node" | "bun" | "python" | "python3"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Agent identification ----

    #[test]
    fn identify_known_agents() {
        assert_eq!(identify_agent("pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("claude"), Some(Agent::Claude));
        assert_eq!(identify_agent("claude-code"), Some(Agent::Claude));
        assert_eq!(identify_agent("codex"), Some(Agent::Codex));
        assert_eq!(identify_agent("gemini"), Some(Agent::Gemini));
        assert_eq!(identify_agent("cursor"), Some(Agent::Cursor));
        assert_eq!(identify_agent("cline"), Some(Agent::Cline));
        assert_eq!(identify_agent("opencode"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("kimi"), Some(Agent::Kimi));
        assert_eq!(identify_agent("ghcs"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("hermes"), Some(Agent::Hermes));
    }

    #[test]
    fn parse_known_agent_labels() {
        assert_eq!(parse_agent_label("pi"), Some(Agent::Pi));
        assert_eq!(parse_agent_label("claude"), Some(Agent::Claude));
        assert_eq!(parse_agent_label("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(
            parse_agent_label("github-copilot"),
            Some(Agent::GithubCopilot)
        );
        assert_eq!(parse_agent_label("amp-local"), Some(Agent::Amp));
    }

    #[test]
    fn agent_labels_use_display_names() {
        assert_eq!(agent_label(Agent::Pi), "pi");
        assert_eq!(agent_label(Agent::GithubCopilot), "copilot");
        assert_eq!(agent_label(Agent::OpenCode), "opencode");
    }

    #[test]
    fn identify_unknown_processes() {
        assert_eq!(identify_agent("bash"), None);
        assert_eq!(identify_agent("zsh"), None);
        assert_eq!(identify_agent("vim"), None);
        assert_eq!(identify_agent("node"), None);
    }

    #[test]
    fn identify_case_insensitive() {
        assert_eq!(identify_agent("Pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("CLAUDE"), Some(Agent::Claude));
        assert_eq!(identify_agent("Codex"), Some(Agent::Codex));
    }

    #[test]
    fn identify_agent_in_job_prefers_wrapped_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![
                crate::platform::ForegroundProcess {
                    pid: 1,
                    name: "node".to_string(),
                    argv0: None,
                    cmdline: Some("node /path/to/bin/codex".to_string()),
                },
                crate::platform::ForegroundProcess {
                    pid: 2,
                    name: "bash".to_string(),
                    argv0: None,
                    cmdline: Some("bash".to_string()),
                },
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    // ---- Workspace state rollup ----

    // ---- No agent → Unknown ----

    #[test]
    fn no_agent_returns_unknown() {
        assert_eq!(detect_state(None, "anything"), AgentState::Unknown);
    }

    // ---- Pi ----

    #[test]
    fn pi_working_when_working() {
        assert_eq!(detect_pi("some output\nWorking..."), AgentState::Working);
    }

    #[test]
    fn pi_working_working_in_middle() {
        assert_eq!(detect_pi("line1\nWorking...\nline3"), AgentState::Working);
    }

    #[test]
    fn pi_idle_at_prompt() {
        assert_eq!(detect_pi("❯ "), AgentState::Idle);
    }

    #[test]
    fn pi_idle_no_working_text() {
        assert_eq!(detect_pi("some output\n\n> ready"), AgentState::Idle);
    }

    // ---- Claude Code ----

    #[test]
    fn claude_working_esc_to_interrupt() {
        let screen = "Reading file src/main.rs\nesc to interrupt\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Working);
    }

    #[test]
    fn claude_working_ctrl_c_to_interrupt() {
        let screen = "Editing code\nctrl+c to interrupt\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Working);
    }

    #[test]
    fn claude_working_spinner() {
        let screen = "✽ Tempering…\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Working);
    }

    #[test]
    fn claude_working_middle_dot_spinner() {
        let screen = "· Thinking…\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Working);
    }

    #[test]
    fn claude_working_spinner_with_detail() {
        let screen = "✳ Simplifying recompute_tangents…\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Working);
    }

    #[test]
    fn claude_waiting_do_you_want() {
        let screen = "Do you want to run this command?\n\nYes  No";
        assert_eq!(detect_claude(screen), AgentState::Blocked);
    }

    #[test]
    fn claude_waiting_would_you_like() {
        let screen = "Would you like to apply these changes?\n\n❯ Yes";
        assert_eq!(detect_claude(screen), AgentState::Blocked);
    }

    #[test]
    fn claude_waiting_selection_prompt() {
        let screen = "Do you want to proceed?\n❯ 1. Yes\n  2. No\n\nEsc to cancel · Tab to amend";
        assert_eq!(detect_claude(screen), AgentState::Blocked);
    }

    #[test]
    fn claude_waiting_esc_to_cancel() {
        let screen = "Allow bash: rm -rf /tmp/test?\n\nDo you want to proceed?\n\nesc to cancel";
        assert_eq!(detect_claude(screen), AgentState::Blocked);
    }

    #[test]
    fn claude_waiting_ask_user_question_menu() {
        let screen =
            "Which approach should I take?\n❯ 1. Minimal change\n  2. Bigger refactor\n3. Chat about this\n\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";
        assert_eq!(detect_claude(screen), AgentState::Blocked);
    }

    #[test]
    fn claude_idle_hooks_menu() {
        let screen = "Hooks\n0 hooks configured\nℹ This menu is read-only. To add or modify hooks, edit settings.json directly or ask Claude. Learn more\n\n❯ 1. PreToolUse\n  2. PostToolUse\n  3. PostToolUseFailure\n\nEnter to confirm · Esc to cancel";
        assert_eq!(detect_claude(screen), AgentState::Idle);
    }

    #[test]
    fn claude_idle_theme_menu() {
        let screen = "Theme\nChoose the text style that looks best with your terminal\n\n❯ 1. Dark mode ✔\n  2. Light mode\n  3. Dark mode (colorblind-friendly)\n\nSyntax theme: Monokai Extended (ctrl+t to disable)\n\nEnter to select · Esc to cancel";
        assert_eq!(detect_claude(screen), AgentState::Idle);
    }

    #[test]
    fn claude_idle_prompt_box() {
        let screen = "Task complete.\n─────────────\n❯ \n─────────────";
        assert_eq!(detect_claude(screen), AgentState::Idle);
    }

    #[test]
    fn claude_idle_search() {
        let screen = "⌕ Search…\nsome content";
        assert_eq!(detect_claude(screen), AgentState::Idle);
    }

    #[test]
    fn claude_working_not_confused_by_old_prompt() {
        // The "esc to interrupt" is ABOVE the prompt box — should be working
        let screen = "✽ Writing…\nesc to interrupt\n──────\n❯ \n──────";
        assert_eq!(detect_claude(screen), AgentState::Working);
    }

    // ---- Codex ----

    #[test]
    fn codex_waiting_confirm() {
        assert_eq!(
            detect_codex("press enter to confirm or esc to cancel"),
            AgentState::Blocked
        );
    }

    #[test]
    fn codex_waiting_allow_command() {
        assert_eq!(detect_codex("allow command?\n[y/n]"), AgentState::Blocked);
    }

    #[test]
    fn codex_waiting_submit_answer() {
        assert_eq!(
            detect_codex("Question about approach\n| enter to submit answer"),
            AgentState::Blocked
        );
    }

    #[test]
    fn codex_waiting_submit_answer_wrapped_footer() {
        assert_eq!(
            detect_codex(
                "Question 1/2\nChoose an option.\nenter to submit answer\nesc to interrupt"
            ),
            AgentState::Blocked
        );
    }

    #[test]
    fn codex_working_interrupt() {
        assert_eq!(
            detect_codex("generating code\nesc to interrupt"),
            AgentState::Working
        );
    }

    #[test]
    fn codex_working_truncated_status_header() {
        assert_eq!(detect_codex("• Working (0s • esc…"), AgentState::Working);
    }

    #[test]
    fn codex_idle() {
        assert_eq!(detect_codex("❯ "), AgentState::Idle);
    }

    // ---- Gemini ----

    #[test]
    fn gemini_waiting_confirmation() {
        assert_eq!(
            detect_gemini("waiting for user confirmation"),
            AgentState::Blocked
        );
    }

    #[test]
    fn gemini_waiting_apply() {
        assert_eq!(
            detect_gemini("│ Apply this change\n│ Yes  │ No"),
            AgentState::Blocked
        );
    }

    #[test]
    fn gemini_waiting_allow_execution() {
        assert_eq!(
            detect_gemini("│ Allow execution of: rm test.txt"),
            AgentState::Blocked
        );
    }

    #[test]
    fn gemini_working() {
        assert_eq!(
            detect_gemini("thinking...\nesc to cancel"),
            AgentState::Working
        );
    }

    #[test]
    fn gemini_idle() {
        assert_eq!(detect_gemini("❯ "), AgentState::Idle);
    }

    // ---- Cursor ----

    #[test]
    fn cursor_waiting_accept() {
        assert_eq!(
            detect_cursor("Apply changes? (y) (enter) or keep (n)"),
            AgentState::Blocked
        );
    }

    #[test]
    fn cursor_waiting_allow() {
        assert_eq!(detect_cursor("allow file edit (y)"), AgentState::Blocked);
    }

    #[test]
    fn cursor_working_spinner() {
        assert_eq!(detect_cursor("⬡ Grepping.."), AgentState::Working);
    }

    #[test]
    fn cursor_working_ctrl_c() {
        assert_eq!(
            detect_cursor("processing\nctrl+c to stop"),
            AgentState::Working
        );
    }

    #[test]
    fn cursor_idle() {
        assert_eq!(detect_cursor("> "), AgentState::Idle);
    }

    // ---- Cline ----

    #[test]
    fn cline_waiting_tool_use() {
        assert_eq!(detect_cline("let cline use this tool"), AgentState::Blocked);
    }

    #[test]
    fn cline_waiting_act_mode() {
        assert_eq!(
            detect_cline("[act mode] execute command?\nyes"),
            AgentState::Blocked
        );
    }

    #[test]
    fn cline_idle_ready() {
        assert_eq!(
            detect_cline("cline is ready for your message"),
            AgentState::Idle
        );
    }

    #[test]
    fn cline_defaults_to_working() {
        // Cline's default is working (unlike other agents)
        assert_eq!(detect_cline("some random output"), AgentState::Working);
    }

    // ---- OpenCode ----

    #[test]
    fn opencode_waiting_permission() {
        assert_eq!(
            detect_opencode("△ Permission required"),
            AgentState::Blocked
        );
    }

    #[test]
    fn opencode_working() {
        assert_eq!(
            detect_opencode("running tool\nesc to interrupt"),
            AgentState::Working
        );
    }

    #[test]
    fn opencode_idle() {
        assert_eq!(detect_opencode("> "), AgentState::Idle);
    }

    // ---- GitHub Copilot ----

    #[test]
    fn copilot_waiting_confirm() {
        assert_eq!(
            detect_github_copilot("confirm with enter"),
            AgentState::Blocked
        );
    }

    #[test]
    fn copilot_waiting_do_you_want() {
        assert_eq!(
            detect_github_copilot("│ do you want to apply?"),
            AgentState::Blocked
        );
    }

    #[test]
    fn copilot_working() {
        assert_eq!(
            detect_github_copilot("generating\nesc to cancel"),
            AgentState::Working
        );
    }

    #[test]
    fn copilot_idle() {
        assert_eq!(detect_github_copilot("> "), AgentState::Idle);
    }

    // ---- Kimi ----

    #[test]
    fn kimi_waiting_approve() {
        assert_eq!(detect_kimi("approve?"), AgentState::Blocked);
    }

    #[test]
    fn kimi_waiting_yn() {
        assert_eq!(detect_kimi("continue? [y/n]"), AgentState::Blocked);
    }

    #[test]
    fn kimi_working_thinking() {
        assert_eq!(detect_kimi("thinking"), AgentState::Working);
    }

    #[test]
    fn kimi_working_generating() {
        assert_eq!(detect_kimi("generating code"), AgentState::Working);
    }

    #[test]
    fn kimi_idle() {
        assert_eq!(detect_kimi("> "), AgentState::Idle);
    }

    // ---- Droid ----

    #[test]
    fn droid_working_thinking_with_spinner() {
        let screen = ">  how u doin\n\n⠴ Thinking...  (Press ESC to stop)\n\nAuto (Off)";
        assert_eq!(detect_droid(screen), AgentState::Working);
    }

    #[test]
    fn droid_working_esc_to_stop_alone() {
        // ESC to stop without spinner is still working (UI chrome)
        let screen = "Processing\n(Press ESC to stop)";
        assert_eq!(detect_droid(screen), AgentState::Working);
    }

    #[test]
    fn droid_waiting_execute_approval() {
        let screen = concat!(
            "⛬  I'll create some folders.\n\n",
            "   EXECUTE  (mkdir -p /tmp/test, impact: medium)\n\n",
            "╭────────────────────╮\n",
            "│ > Yes, allow        │\n",
            "│   Yes, always allow │\n",
            "│   No, cancel        │\n",
            "╰────────────────────╯\n",
            "   Use ↑↓ to navigate, Enter to select, Esc to cancel\n",
        );
        assert_eq!(detect_droid(screen), AgentState::Blocked);
    }

    #[test]
    fn droid_waiting_selection_with_chrome() {
        let screen = "│ > Yes, allow │\n│   No, cancel │\n   Use ↑↓ to navigate, Enter to select, Esc to cancel";
        assert_eq!(detect_droid(screen), AgentState::Blocked);
    }

    #[test]
    fn droid_not_waiting_on_options_text_alone() {
        // "Yes, allow" in normal conversation should NOT trigger blocked
        let screen = "The user said > Yes, allow the changes";
        assert_eq!(detect_droid(screen), AgentState::Idle);
    }

    #[test]
    fn droid_idle_prompt() {
        let screen =
            "╭──────────────────╮\n│ > Try something   │\n╰──────────────────╯\n? for help";
        assert_eq!(detect_droid(screen), AgentState::Idle);
    }

    #[test]
    fn droid_idle_after_response() {
        let screen =
            "⛬  Doing well, thanks!\n\nAuto (Off)\n╭──────────╮\n│ >        │\n╰──────────╯";
        assert_eq!(detect_droid(screen), AgentState::Idle);
    }

    #[test]
    fn droid_mission_control_is_idle_even_with_stop_chrome() {
        let screen = concat!(
            "⛬ Mission Control  ~/Projects/herdr-worktrees/persistence  TIME 22h 40m · Input 12.6M · Cached 129.7M · Output 471.5K\n",
            "●  RUNNING\n",
            "Active Feature  fix-scrutiny-misc-followups\n",
            "Features  25/28\n",
            "(Press ESC to stop)\n",
        );
        assert_eq!(detect_droid(screen), AgentState::Idle);
    }

    #[test]
    fn droid_leaves_mission_control_and_returns_to_working() {
        let screen = ">  how u doin\n\n⠴ Thinking...  (Press ESC to stop)\n\nAuto (Off)";
        assert_eq!(detect_droid(screen), AgentState::Working);
    }

    #[test]
    fn droid_braille_spinner_detected() {
        assert!(has_braille_spinner("⠴ Thinking..."));
        assert!(has_braille_spinner("  ⠧ Loading..."));
        assert!(has_braille_spinner("text\n⠋ Working\nmore"));
    }

    #[test]
    fn droid_braille_spinner_no_false_positive() {
        assert!(!has_braille_spinner("normal text"));
        assert!(!has_braille_spinner("Thinking..."));
        assert!(!has_braille_spinner("some ⠴ in middle of text"));
    }

    #[test]
    fn droid_identified_by_process_name() {
        assert_eq!(identify_agent("droid"), Some(Agent::Droid));
    }

    // ---- Amp ----

    #[test]
    fn amp_blocked_waiting_for_approval() {
        let screen = "Invoke tool shell_command?\n▸● Approve [Alt+1]\n ○ Allow All for This Session [Alt+2]\n ○ Allow All for Every Session [Alt+3]\n ○ Deny with feedback [Alt+4]\nWaiting for approval...";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Blocked);
    }

    #[test]
    fn amp_blocked_run_this_command() {
        let screen = "Run this command?\nrg --files\n▸● Approve [Alt+1]\n ○ Allow All for This Session [Alt+2]\n ○ Allow All for Every Session [Alt+3]\n ○ Deny with feedback [Alt+4]";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Blocked);
    }

    #[test]
    fn amp_blocked_allow_editing_file() {
        let screen = "Allow editing file:\nsrc/detect.rs\n▸● Approve [Alt+1]\n ○ Allow File for Every Session [Alt+2]\n ○ Allow All for This Session [Alt+3]\n ○ Deny with feedback [Alt+4]";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Blocked);
    }

    #[test]
    fn amp_blocked_allow_creating_file() {
        let screen = "Allow creating file:\nsrc/new_file.rs\n▸● Approve [Alt+1]\n ○ Allow File for Every Session [Alt+2]\n ○ Allow All for This Session [Alt+3]\n ○ Deny with feedback [Alt+4]";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Blocked);
    }

    #[test]
    fn amp_working_running_tools() {
        let screen = "  ✓ Search Map the core runtime architecture\n  ⋯ Oracle ▼\n  ≈ Running tools...         Esc to cancel";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Working);
    }

    #[test]
    fn amp_idle() {
        let screen = "  Response complete.\n\n╭─100% of 272k · $1.20─────────────────────────╮\n│                                               │\n╰───────────────────────~/Projects/herdr (master)╯";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Idle);
    }

    #[test]
    fn amp_identified_by_process_name() {
        assert_eq!(identify_agent("amp"), Some(Agent::Amp));
        assert_eq!(identify_agent("amp-local"), Some(Agent::Amp));
    }

    #[test]
    fn hermes_identified_by_process_name() {
        assert_eq!(identify_agent("hermes"), Some(Agent::Hermes));
    }

    #[test]
    fn hermes_idle_at_prompt() {
        let screen = "⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 0s\n\n────────────────────\n❯ \n────────────────────";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    #[test]
    fn hermes_working_when_thinking_spinner_visible() {
        let screen = "(｡•́︿•̀｡) pondering...\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 2s\n\n────────────────────\n❯ \n────────────────────";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Working
        );
    }

    #[test]
    fn hermes_working_when_tool_spinner_visible() {
        let screen = "🖥 terminal ls -la  (0.4s)\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 3s\n\n────────────────────\n❯ \n────────────────────";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Working
        );
    }

    #[test]
    fn hermes_working_when_terminal_tool_progress_visible() {
        let screen = "💻 $         pwd && echo '---'  0.3s\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 3s\n\n────────────────────\n❯ \n────────────────────";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Working
        );
    }

    #[test]
    fn hermes_working_when_execute_code_progress_visible() {
        let screen = "🐍 exec      from pathlib import Path  3.2s\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 3s\n\n────────────────────\n❯ \n────────────────────";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Working
        );
    }

    #[test]
    fn hermes_working_when_skill_tool_progress_visible() {
        let screen = "📚 skill     code-review  0.0s\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 3s\n\n────────────────────\n❯ \n────────────────────";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Working
        );
    }

    #[test]
    fn hermes_blocked_on_approval_prompt() {
        let screen = "╭────────────────────────────────────────────╮\n│ Dangerous Command                          │\n│ rm -rf /tmp/demo                           │\n│ ❯ Allow once                               │\n│   Allow for this session                   │\n│   Add to permanent allowlist               │\n│   Deny                                     │\n╰────────────────────────────────────────────╯";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Blocked
        );
    }

    #[test]
    fn hermes_blocked_on_clarify_prompt() {
        let screen = "╭────────────────────────────────────────────╮\n│ Hermes needs your input                    │\n│ Which environment should I use?            │\n│ ❯ prod                                     │\n│   staging                                  │\n╰────────────────────────────────────────────╯\n  ↑/↓ to select, Enter to confirm";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Blocked
        );
    }

    #[test]
    fn hermes_resume_recap_without_prompt_is_idle() {
        let screen = "↻ Resumed session 20260416_071429_2b732a \"Fixing Hermes Agent Autodetection\" (9 user messages, 338 total messages)\n╭─────────────────────────────────────────────── Previous Conversation ────────────────────────────────────────────────╮\n│   ... 119 earlier messages ...                                                                                       │\n│   ● You: okay lets test it                                                                                           │\n│   ◆ Hermes: Yep — I found the actual cause and patched it.                                                           │\n╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯\nWelcome to Hermes Agent! Type your message or /help for commands.";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    #[test]
    fn hermes_ignores_stale_tool_spinner_above_prompt() {
        let screen = "🖥 terminal ls -la  (0.4s)\nold output\nmore old output\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 0s\n────────────────────\n❯ \n────────────────────";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    #[test]
    fn hermes_search_output_with_blocked_keywords_is_not_blocked() {
        let screen = "🔎 grep      Hermes needs your input|Enter password below|Dangerous Command|Allow once\nmatch 1\nmatch 2\n⚕ gpt-5.4 │ ctx -- │ [░░░░░░░░░░] -- │ 0s\n────────────────────\n❯ \n────────────────────";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    // ---- Helpers ----

    #[test]
    fn content_above_prompt_box_extracts_correctly() {
        let screen = "line1\nline2\n──────\n❯ \n──────";
        let above = content_above_prompt_box(screen);
        assert!(above.contains("line1"));
        assert!(above.contains("line2"));
        assert!(!above.contains('❯'));
    }

    #[test]
    fn content_above_prompt_box_no_box() {
        let screen = "just some text\nno borders here";
        let above = content_above_prompt_box(screen);
        assert_eq!(above, screen);
    }

    #[test]
    fn spinner_activity_detected() {
        assert!(has_spinner_activity("· Thinking…"));
        assert!(has_spinner_activity("✽ Tempering…"));
        assert!(has_spinner_activity("✳ Simplifying recompute_tangents…"));
        assert!(has_spinner_activity("  ✶ Reading…")); // with leading whitespace
        assert!(has_spinner_activity("✻ Pouncing…"));
        assert!(has_spinner_activity("✽ Processing…"));
    }

    #[test]
    fn spinner_activity_not_false_positive() {
        assert!(!has_spinner_activity("normal text"));
        assert!(!has_spinner_activity("✽ no ellipsis here"));
        assert!(!has_spinner_activity("✽ …"));
        assert!(!has_spinner_activity("some ✽ in the middle"));
    }

    #[test]
    fn cursor_spinner_detected() {
        assert!(has_cursor_spinner("⬡ Grepping.."));
        assert!(has_cursor_spinner("⬢ Reading…"));
    }

    #[test]
    fn cursor_spinner_not_false_positive() {
        assert!(!has_cursor_spinner("normal text"));
        assert!(!has_cursor_spinner("some ⬡ in middle"));
    }

    // ---- Process identification (real PTY) ----

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_job_detects_sleep() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("failed to open pty");

        // Spawn "sleep 999" — a known, deterministic process
        let mut cmd = CommandBuilder::new("sleep");
        cmd.arg("999");
        let mut child = pair.slave.spawn_command(cmd).expect("failed to spawn");
        let pid = child.process_id().expect("no pid");

        // Give the process a moment to become the foreground group
        std::thread::sleep(std::time::Duration::from_millis(50));

        let job = foreground_job(pid).expect("expected foreground job");
        assert!(
            job.processes.iter().any(|p| p.name == "sleep"),
            "expected sleep in {job:?}"
        );
        assert_eq!(
            identify_agent_in_job(&job),
            None,
            "sleep should not map to an agent"
        );

        // Clean up
        child.kill().ok();
        child.wait().ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_job_detects_shell_running_command() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::Write;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("failed to open pty");

        // Spawn a shell, then run a command inside it
        let cmd = CommandBuilder::new("sh");
        let mut child = pair.slave.spawn_command(cmd).expect("failed to spawn");
        let pid = child.process_id().expect("no pid");

        // Write a command to the shell
        let mut writer = pair.master.take_writer().expect("no writer");
        // Use exec so sleep replaces sh as the foreground process
        writer.write_all(b"exec sleep 999\n").ok();
        drop(writer);

        std::thread::sleep(std::time::Duration::from_millis(100));

        let job = foreground_job(pid).expect("expected foreground job");
        assert!(
            job.processes.iter().any(|p| p.name == "sleep"),
            "expected sleep in {job:?}"
        );
        assert_eq!(
            identify_agent_in_job(&job),
            None,
            "sleep should not map to an agent"
        );

        child.kill().ok();
        child.wait().ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_parsing_handles_spaces_in_comm() {
        // Verify our /proc/pid/stat parser correctly extracts fields
        // even when (comm) could contain spaces.
        let pid = std::process::id();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();

        // Our parsing: find last ')' then split the rest
        let close_paren = stat.rfind(')').expect("should have closing paren");
        let rest = &stat[close_paren + 2..];
        let fields: Vec<&str> = rest.split_whitespace().collect();

        // We should have enough fields (at least 6 for tpgid)
        assert!(
            fields.len() >= 6,
            "not enough fields in stat: {}",
            fields.len()
        );

        // Field 0 should be a valid state char (S, R, D, etc.)
        let state = fields[0];
        assert!(
            ["S", "R", "D", "Z", "T", "t", "W", "X", "I"].contains(&state),
            "unexpected state: {state}"
        );

        // Field 5 (tpgid) should parse as i32 (can be -1 if no controlling terminal)
        let tpgid: i32 = fields[5].parse().expect("tpgid should be a number");
        // In CI/test environments without a terminal, tpgid is typically -1
        let _ = tpgid;
    }

    #[test]
    fn terminal_tail_content_works_with_detection() {
        assert_eq!(detect_pi("Working..."), AgentState::Working);
    }

    #[test]
    fn ansi_colored_content_still_detects_working() {
        assert_eq!(detect_pi("\x1b[31mWorking...\x1b[0m"), AgentState::Working);
    }

    #[test]
    fn visible_claude_prompt_box_is_idle() {
        let screen = "Task complete.\n─────────────\n❯ \n─────────────";
        assert_eq!(detect_claude(screen), AgentState::Idle);
    }
}
