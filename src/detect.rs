//! Agent state detection via screen content pattern matching.
//!
//! Each pane's `vt100::Screen` content is read periodically and matched
//! against known agent output patterns to determine state.

/// The detected state of a terminal pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Agent finished, prompt visible, nothing happening.
    Idle,
    /// Agent is actively working/processing.
    Busy,
    /// Agent needs human input — approval, question, error.
    Waiting,
    /// Plain shell or unrecognized program.
    Unknown,
}

impl AgentState {
    /// Priority for rolling up pane states to a workspace indicator.
    /// Higher = more urgent.
    pub fn priority(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Idle => 1,
            Self::Busy => 2,
            Self::Waiting => 3,
        }
    }
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

/// Detect the state of an agent from the visible screen content.
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
    }
}

/// Roll up multiple pane states into a single workspace state.
/// Returns the highest-priority state among all panes.
pub fn workspace_state(pane_states: &[AgentState]) -> AgentState {
    pane_states
        .iter()
        .max_by_key(|s| s.priority())
        .copied()
        .unwrap_or(AgentState::Unknown)
}

// ---------------------------------------------------------------------------
// Per-agent detectors
// ---------------------------------------------------------------------------

fn detect_pi(content: &str) -> AgentState {
    // pi shows "Working..." when the agent is processing
    if content.contains("Working...") {
        return AgentState::Busy;
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

    // --- Waiting detection (full content including prompt box) ---

    // "Do you want" or "Would you like" followed by yes/❯
    if has_confirmation_prompt(&lower) {
        return AgentState::Waiting;
    }

    // Selection prompt: ❯ followed by numbered option
    if has_selection_prompt(content) {
        return AgentState::Waiting;
    }

    // "esc to cancel" indicates permission/confirmation dialog
    if lower.contains("esc to cancel") {
        return AgentState::Waiting;
    }

    // --- Busy detection (content above the prompt box) ---

    let above = content_above_prompt_box(content);
    let above_lower = above.to_lowercase();

    if above_lower.contains("esc to interrupt") || above_lower.contains("ctrl+c to interrupt") {
        return AgentState::Busy;
    }

    if has_spinner_activity(above) {
        return AgentState::Busy;
    }

    AgentState::Idle
}

fn detect_codex(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting patterns
    if lower.contains("press enter to confirm or esc to cancel")
        || lower.contains("| enter to submit answer")
        || lower.contains("allow command?")
        || lower.contains("[y/n]")
        || lower.contains("yes (y)")
    {
        return AgentState::Waiting;
    }
    if has_confirmation_prompt(&lower) {
        return AgentState::Waiting;
    }

    // Busy
    if has_interrupt_pattern(&lower) {
        return AgentState::Busy;
    }

    AgentState::Idle
}

fn detect_gemini(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting — explicit confirmation
    if lower.contains("waiting for user confirmation") {
        return AgentState::Waiting;
    }

    // Waiting — box-drawing confirmation prompts
    if content.contains("│ Apply this change")
        || content.contains("│ Allow execution")
        || content.contains("│ Do you want to proceed")
    {
        return AgentState::Waiting;
    }
    if has_confirmation_prompt(&lower) {
        return AgentState::Waiting;
    }

    // Busy
    if lower.contains("esc to cancel") {
        return AgentState::Busy;
    }

    AgentState::Idle
}

fn detect_cursor(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting
    if lower.contains("(y) (enter)")
        || lower.contains("keep (n)")
        || lower.contains("skip (esc or n)")
    {
        return AgentState::Waiting;
    }
    // "allow ...(y)" or "run ...(y)" patterns
    if lower.contains("(y)") && (lower.contains("allow") || lower.contains("run")) {
        return AgentState::Waiting;
    }

    // Busy
    if lower.contains("ctrl+c to stop") {
        return AgentState::Busy;
    }
    if has_cursor_spinner(content) {
        return AgentState::Busy;
    }

    AgentState::Idle
}

fn detect_cline(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting
    if lower.contains("let cline use this tool") {
        return AgentState::Waiting;
    }
    // [act mode] or [plan mode] followed by "yes"
    if (lower.contains("[act mode]") || lower.contains("[plan mode]")) && lower.contains("yes") {
        return AgentState::Waiting;
    }

    // Idle
    if lower.contains("cline is ready for your message") {
        return AgentState::Idle;
    }

    // Cline defaults to busy (unlike most agents that default to idle)
    AgentState::Busy
}

fn detect_opencode(content: &str) -> AgentState {
    // Waiting
    if content.contains("△ Permission required") {
        return AgentState::Waiting;
    }

    // Busy
    if has_interrupt_pattern(&content.to_lowercase()) {
        return AgentState::Busy;
    }

    AgentState::Idle
}

fn detect_github_copilot(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting
    if lower.contains("│ do you want") {
        return AgentState::Waiting;
    }
    if lower.contains("confirm with") && lower.contains("enter") {
        return AgentState::Waiting;
    }

    // Busy
    if lower.contains("esc to cancel") {
        return AgentState::Busy;
    }

    AgentState::Idle
}

fn detect_kimi(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting
    if lower.contains("allow?")
        || lower.contains("confirm?")
        || lower.contains("approve?")
        || lower.contains("proceed?")
        || lower.contains("[y/n]")
        || lower.contains("(y/n)")
    {
        return AgentState::Waiting;
    }

    // Busy
    if lower.contains("thinking")
        || lower.contains("processing")
        || lower.contains("generating")
        || lower.contains("waiting for response")
        || lower.contains("ctrl+c to cancel")
        || lower.contains("ctrl-c to cancel")
    {
        return AgentState::Busy;
    }

    AgentState::Idle
}

/// Droid detection.
///
/// Busy: braille spinner line (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) + "Thinking..." + "(Press ESC to stop)"
/// Waiting: EXECUTE prompt with selection box ("Yes, allow" / "No, cancel") +
///          "Use ↑↓ to navigate, Enter to select"
/// Idle: prompt box visible, no spinner, no selection prompt
fn detect_droid(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Waiting: EXECUTE approval prompt with selection UI chrome
    // Primary (AND): structural keyword + chrome text = certain
    let has_execute = content.contains("EXECUTE");
    let has_selection_chrome = lower.contains("enter to select")
        || lower.contains("↑↓ to navigate")
        || lower.contains("esc to cancel");
    let has_selection_options = lower.contains("> yes, allow") || lower.contains("> no, cancel");

    if has_execute && (has_selection_chrome || has_selection_options) {
        return AgentState::Waiting;
    }
    // Secondary: selection chrome + options together (no EXECUTE needed)
    if has_selection_chrome && has_selection_options {
        return AgentState::Waiting;
    }

    // Busy: braille spinner character at start of a line + "Thinking..."
    // The braille chars (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) are very specific — won't appear in normal content
    if has_braille_spinner(content) && lower.contains("esc to stop") {
        return AgentState::Busy;
    }
    // Fallback: "ESC to stop" alone is still a strong signal (it's UI chrome)
    if lower.contains("esc to stop") {
        return AgentState::Busy;
    }

    AgentState::Idle
}

/// Amp (Sourcegraph) detection.
///
/// Screen layout when busy:
/// ```text
///   ✓ Search Map the core runtime architecture...
///   ⋯ Oracle ▼
///   ≈ Running tools...         Esc to cancel
/// ```
///
/// "Esc to cancel" is the reliable busy indicator — it only appears
/// while amp is actively running tools or thinking.
fn detect_amp(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    if lower.contains("esc to cancel") {
        return AgentState::Busy;
    }

    AgentState::Idle
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
fn has_spinner_activity(content: &str) -> bool {
    const SPINNER_CHARS: &str = "✱✲✳✴✵✶✷✸✹✺✻✼✽✾✿❀❁❂❃❇❈❉❊❋✢✣✤✥✦✧✨⊛⊕⊙◉◎◍⁂⁕※⍟☼★☆";
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

    #[test]
    fn workspace_state_waiting_wins() {
        let states = [AgentState::Idle, AgentState::Busy, AgentState::Waiting];
        assert_eq!(workspace_state(&states), AgentState::Waiting);
    }

    #[test]
    fn workspace_state_busy_over_idle() {
        let states = [AgentState::Idle, AgentState::Busy, AgentState::Unknown];
        assert_eq!(workspace_state(&states), AgentState::Busy);
    }

    #[test]
    fn workspace_state_empty() {
        assert_eq!(workspace_state(&[]), AgentState::Unknown);
    }

    // ---- No agent → Unknown ----

    #[test]
    fn no_agent_returns_unknown() {
        assert_eq!(detect_state(None, "anything"), AgentState::Unknown);
    }

    // ---- Pi ----

    #[test]
    fn pi_busy_when_working() {
        assert_eq!(detect_pi("some output\nWorking..."), AgentState::Busy);
    }

    #[test]
    fn pi_busy_working_in_middle() {
        assert_eq!(detect_pi("line1\nWorking...\nline3"), AgentState::Busy);
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
    fn claude_busy_esc_to_interrupt() {
        let screen = "Reading file src/main.rs\nesc to interrupt\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Busy);
    }

    #[test]
    fn claude_busy_ctrl_c_to_interrupt() {
        let screen = "Editing code\nctrl+c to interrupt\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Busy);
    }

    #[test]
    fn claude_busy_spinner() {
        let screen = "✽ Tempering…\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Busy);
    }

    #[test]
    fn claude_busy_spinner_with_detail() {
        let screen = "✳ Simplifying recompute_tangents…\n─────────\n❯ \n─────────";
        assert_eq!(detect_claude(screen), AgentState::Busy);
    }

    #[test]
    fn claude_waiting_do_you_want() {
        let screen = "Do you want to run this command?\n\nYes  No";
        assert_eq!(detect_claude(screen), AgentState::Waiting);
    }

    #[test]
    fn claude_waiting_would_you_like() {
        let screen = "Would you like to apply these changes?\n\n❯ Yes";
        assert_eq!(detect_claude(screen), AgentState::Waiting);
    }

    #[test]
    fn claude_waiting_selection_prompt() {
        let screen = "Choose an option:\n❯ 1. Apply\n  2. Skip\n  3. Cancel";
        assert_eq!(detect_claude(screen), AgentState::Waiting);
    }

    #[test]
    fn claude_waiting_esc_to_cancel() {
        let screen = "Allow bash: rm -rf /tmp/test?\n\nesc to cancel";
        assert_eq!(detect_claude(screen), AgentState::Waiting);
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
    fn claude_busy_not_confused_by_old_prompt() {
        // The "esc to interrupt" is ABOVE the prompt box — should be busy
        let screen = "✽ Writing…\nesc to interrupt\n──────\n❯ \n──────";
        assert_eq!(detect_claude(screen), AgentState::Busy);
    }

    // ---- Codex ----

    #[test]
    fn codex_waiting_confirm() {
        assert_eq!(
            detect_codex("press enter to confirm or esc to cancel"),
            AgentState::Waiting
        );
    }

    #[test]
    fn codex_waiting_allow_command() {
        assert_eq!(detect_codex("allow command?\n[y/n]"), AgentState::Waiting);
    }

    #[test]
    fn codex_waiting_submit_answer() {
        assert_eq!(
            detect_codex("Question about approach\n| enter to submit answer"),
            AgentState::Waiting
        );
    }

    #[test]
    fn codex_busy_interrupt() {
        assert_eq!(
            detect_codex("generating code\nesc to interrupt"),
            AgentState::Busy
        );
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
            AgentState::Waiting
        );
    }

    #[test]
    fn gemini_waiting_apply() {
        assert_eq!(
            detect_gemini("│ Apply this change\n│ Yes  │ No"),
            AgentState::Waiting
        );
    }

    #[test]
    fn gemini_waiting_allow_execution() {
        assert_eq!(
            detect_gemini("│ Allow execution of: rm test.txt"),
            AgentState::Waiting
        );
    }

    #[test]
    fn gemini_busy() {
        assert_eq!(
            detect_gemini("thinking...\nesc to cancel"),
            AgentState::Busy
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
            AgentState::Waiting
        );
    }

    #[test]
    fn cursor_waiting_allow() {
        assert_eq!(detect_cursor("allow file edit (y)"), AgentState::Waiting);
    }

    #[test]
    fn cursor_busy_spinner() {
        assert_eq!(detect_cursor("⬡ Grepping.."), AgentState::Busy);
    }

    #[test]
    fn cursor_busy_ctrl_c() {
        assert_eq!(
            detect_cursor("processing\nctrl+c to stop"),
            AgentState::Busy
        );
    }

    #[test]
    fn cursor_idle() {
        assert_eq!(detect_cursor("> "), AgentState::Idle);
    }

    // ---- Cline ----

    #[test]
    fn cline_waiting_tool_use() {
        assert_eq!(detect_cline("let cline use this tool"), AgentState::Waiting);
    }

    #[test]
    fn cline_waiting_act_mode() {
        assert_eq!(
            detect_cline("[act mode] execute command?\nyes"),
            AgentState::Waiting
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
    fn cline_defaults_to_busy() {
        // Cline's default is busy (unlike other agents)
        assert_eq!(detect_cline("some random output"), AgentState::Busy);
    }

    // ---- OpenCode ----

    #[test]
    fn opencode_waiting_permission() {
        assert_eq!(
            detect_opencode("△ Permission required"),
            AgentState::Waiting
        );
    }

    #[test]
    fn opencode_busy() {
        assert_eq!(
            detect_opencode("running tool\nesc to interrupt"),
            AgentState::Busy
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
            AgentState::Waiting
        );
    }

    #[test]
    fn copilot_waiting_do_you_want() {
        assert_eq!(
            detect_github_copilot("│ do you want to apply?"),
            AgentState::Waiting
        );
    }

    #[test]
    fn copilot_busy() {
        assert_eq!(
            detect_github_copilot("generating\nesc to cancel"),
            AgentState::Busy
        );
    }

    #[test]
    fn copilot_idle() {
        assert_eq!(detect_github_copilot("> "), AgentState::Idle);
    }

    // ---- Kimi ----

    #[test]
    fn kimi_waiting_approve() {
        assert_eq!(detect_kimi("approve?"), AgentState::Waiting);
    }

    #[test]
    fn kimi_waiting_yn() {
        assert_eq!(detect_kimi("continue? [y/n]"), AgentState::Waiting);
    }

    #[test]
    fn kimi_busy_thinking() {
        assert_eq!(detect_kimi("thinking"), AgentState::Busy);
    }

    #[test]
    fn kimi_busy_generating() {
        assert_eq!(detect_kimi("generating code"), AgentState::Busy);
    }

    #[test]
    fn kimi_idle() {
        assert_eq!(detect_kimi("> "), AgentState::Idle);
    }

    // ---- Droid ----

    #[test]
    fn droid_busy_thinking_with_spinner() {
        let screen = ">  how u doin\n\n⠴ Thinking...  (Press ESC to stop)\n\nAuto (Off)";
        assert_eq!(detect_droid(screen), AgentState::Busy);
    }

    #[test]
    fn droid_busy_esc_to_stop_alone() {
        // ESC to stop without spinner is still busy (UI chrome)
        let screen = "Processing\n(Press ESC to stop)";
        assert_eq!(detect_droid(screen), AgentState::Busy);
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
        assert_eq!(detect_droid(screen), AgentState::Waiting);
    }

    #[test]
    fn droid_waiting_selection_with_chrome() {
        let screen = "│ > Yes, allow │\n│   No, cancel │\n   Use ↑↓ to navigate, Enter to select, Esc to cancel";
        assert_eq!(detect_droid(screen), AgentState::Waiting);
    }

    #[test]
    fn droid_not_waiting_on_options_text_alone() {
        // "Yes, allow" in normal conversation should NOT trigger waiting
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
    fn amp_busy_running_tools() {
        let screen = "  ✓ Search Map the core runtime architecture\n  ⋯ Oracle ▼\n  ≈ Running tools...         Esc to cancel";
        assert_eq!(detect_state(Some(Agent::Amp), screen), AgentState::Busy);
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

    // ---- VT100 integration ----

    #[test]
    fn vt100_screen_content_works_with_detection() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working...");
        let content = parser.screen().contents();
        assert_eq!(detect_pi(&content), AgentState::Busy);
    }

    #[test]
    fn vt100_screen_with_ansi_colors() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        // Red "Working..." with ANSI codes — contents() strips formatting
        parser.process(b"\x1b[31mWorking...\x1b[0m");
        let content = parser.screen().contents();
        assert_eq!(detect_pi(&content), AgentState::Busy);
    }

    #[test]
    fn vt100_screen_claude_prompt_box() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        let screen = "Task complete.\n─────────────\n❯ \n─────────────";
        parser.process(screen.as_bytes());
        let content = parser.screen().contents();
        assert_eq!(detect_claude(&content), AgentState::Idle);
    }
}
