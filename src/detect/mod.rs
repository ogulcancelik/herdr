//! Agent state detection via terminal tail pattern matching.
//!
//! Each pane's live bottom-of-buffer text is read periodically and matched
//! against known agent output patterns to determine state.

mod agents;

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

/// Screen-derived agent state plus confidence metadata used for source arbitration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDetection {
    pub state: AgentState,
    /// True when the current screen is an agent-owned viewer that shows
    /// transcript/history instead of the live prompt state.
    pub skip_state_update: bool,
    /// True when the current screen visibly shows live UI chrome that needs
    /// human input. This is stronger than arbitrary prompt-like text in the
    /// scrollback and may override a non-blocked integration state.
    pub visible_blocker: bool,
    /// True when the current screen visibly shows the agent's idle input UI.
    /// This lets Herdr recover from integrations that miss an interrupt/stop
    /// event without treating an empty or ambiguous screen as idle authority.
    pub visible_idle: bool,
    /// True when the current screen visibly shows live working chrome. This is
    /// narrower than a fallback `Working` heuristic and may guard against stale
    /// hook idle reports.
    pub visible_working: bool,
}

/// Which agent we detected running in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Pi,
    Claude,
    Codex,
    Gemini,
    Cursor,
    Antigravity,
    Cline,
    OpenCode,
    GithubCopilot,
    Kimi,
    Kiro,
    Droid,
    Amp,
    Grok,
    Hermes,
    Kilo,
    Qodercli,
}

pub fn agent_label(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "pi",
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Gemini => "gemini",
        Agent::Cursor => "cursor",
        Agent::Antigravity => "agy",
        Agent::Cline => "cline",
        Agent::OpenCode => "opencode",
        Agent::GithubCopilot => "copilot",
        Agent::Kimi => "kimi",
        Agent::Kiro => "kiro",
        Agent::Droid => "droid",
        Agent::Amp => "amp",
        Agent::Grok => "grok",
        Agent::Hermes => "hermes",
        Agent::Kilo => "kilo",
        Agent::Qodercli => "qodercli",
    }
}

pub fn parse_agent_label(agent: &str) -> Option<Agent> {
    let name = agent.trim().to_lowercase();
    match name.as_str() {
        "pi" => Some(Agent::Pi),
        "claude" | "claude-code" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        "gemini" => Some(Agent::Gemini),
        "cursor" | "cursor-agent" => Some(Agent::Cursor),
        "agy" | "antigravity" | "antigravity-cli" => Some(Agent::Antigravity),
        "cline" => Some(Agent::Cline),
        "opencode" | "open-code" => Some(Agent::OpenCode),
        "copilot" | "github-copilot" | "ghcs" => Some(Agent::GithubCopilot),
        "kimi" | "kimi-code" | "kimi code" => Some(Agent::Kimi),
        "kiro" | "kiro-cli" => Some(Agent::Kiro),
        "droid" => Some(Agent::Droid),
        "amp" | "amp-local" => Some(Agent::Amp),
        "grok" | "grok-build" => Some(Agent::Grok),
        "hermes" | "hermes-agent" => Some(Agent::Hermes),
        "kilo" | "kilo-code" | "kilo code" => Some(Agent::Kilo),
        "qodercli" | "qoderclicn" | "qoder" | "qodercn" => Some(Agent::Qodercli),
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
        "cursor" | "cursor-agent" => Some(Agent::Cursor),
        "agy" | "antigravity" | "antigravity-cli" => Some(Agent::Antigravity),
        "cline" => Some(Agent::Cline),
        "opencode" | "open-code" => Some(Agent::OpenCode),
        "copilot" | "github-copilot" | "ghcs" => Some(Agent::GithubCopilot),
        "kimi" | "kimi-code" | "kimi code" => Some(Agent::Kimi),
        "kiro" | "kiro-cli" => Some(Agent::Kiro),
        "droid" => Some(Agent::Droid),
        "amp" | "amp-local" => Some(Agent::Amp),
        "grok" | "grok-build" => Some(Agent::Grok),
        "hermes" | "hermes-agent" => Some(Agent::Hermes),
        "kilo" | "kilo-code" | "kilo code" => Some(Agent::Kilo),
        "qodercli" | "qoderclicn" | "qoder" | "qodercn" => Some(Agent::Qodercli),
        _ => None,
    }
}

pub fn identify_agent_in_job(job: &crate::platform::ForegroundJob) -> Option<(Agent, String)> {
    if let Some(process) = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)
    {
        let candidate = normalized_process_name(process);
        if let Some(agent) = identify_agent(&candidate) {
            return Some((agent, candidate));
        }
    }

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
#[cfg(test)]
pub fn detect_state(agent: Option<Agent>, screen_content: &str) -> AgentState {
    detect_agent(agent, screen_content).state
}

/// Detect state and whether a visible blocker is present on the current screen.
pub fn detect_agent(agent: Option<Agent>, screen_content: &str) -> AgentDetection {
    let Some(agent) = agent else {
        return AgentDetection {
            state: AgentState::Unknown,
            skip_state_update: false,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
        };
    };
    agents::detect(agent, screen_content)
}

pub fn should_skip_state_update(agent: Option<Agent>, screen_content: &str) -> bool {
    agent.is_some_and(|agent| agents::should_skip_state_update(agent, screen_content))
}

// ---------------------------------------------------------------------------
// Per-agent detectors
// ---------------------------------------------------------------------------

#[cfg(test)]
fn detect_pi(content: &str) -> AgentState {
    detect_state(Some(Agent::Pi), content)
}

#[cfg(test)]
fn detect_claude(content: &str) -> AgentState {
    detect_state(Some(Agent::Claude), content)
}

#[cfg(test)]
fn detect_codex(content: &str) -> AgentState {
    detect_state(Some(Agent::Codex), content)
}

#[cfg(test)]
fn detect_gemini(content: &str) -> AgentState {
    detect_state(Some(Agent::Gemini), content)
}

#[cfg(test)]
fn detect_cursor(content: &str) -> AgentState {
    detect_state(Some(Agent::Cursor), content)
}

#[cfg(test)]
fn detect_antigravity(content: &str) -> AgentState {
    detect_state(Some(Agent::Antigravity), content)
}

#[cfg(test)]
fn detect_cline(content: &str) -> AgentState {
    detect_state(Some(Agent::Cline), content)
}

#[cfg(test)]
fn detect_opencode(content: &str) -> AgentState {
    detect_state(Some(Agent::OpenCode), content)
}

#[cfg(test)]
fn detect_github_copilot(content: &str) -> AgentState {
    detect_state(Some(Agent::GithubCopilot), content)
}

#[cfg(test)]
fn detect_kimi(content: &str) -> AgentState {
    detect_state(Some(Agent::Kimi), content)
}

#[cfg(test)]
fn detect_droid(content: &str) -> AgentState {
    detect_state(Some(Agent::Droid), content)
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

#[cfg(test)]
fn detect_qodercli(content: &str) -> AgentState {
    detect_state(Some(Agent::Qodercli), content)
}

#[cfg(test)]
fn detect_kilo(content: &str) -> AgentState {
    detect_state(Some(Agent::Kilo), content)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Check for action confirmations followed by "yes" or "❯".
fn has_confirmation_prompt(lower_content: &str) -> bool {
    if let Some(pos) = lower_content
        .find("do you want to")
        .or_else(|| lower_content.find("would you like to"))
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

/// Check for real interrupt hints, not unrelated "esc" and "interrupt" text.
fn has_interrupt_pattern(lower_content: &str) -> bool {
    lower_content.contains("esc to interrupt")
        || lower_content.contains("ctrl+c to interrupt")
        || lower_content.contains("press esc to interrupt")
}

#[cfg(test)]
fn has_cursor_spinner(content: &str) -> bool {
    agents::cursor::has_cursor_spinner(content)
}

#[cfg(test)]
fn content_above_prompt_box(content: &str) -> &str {
    agents::claude_code::content_above_prompt_box(content)
}

#[cfg(test)]
fn has_spinner_activity(content: &str) -> bool {
    agents::claude_code::has_spinner_activity(content)
}

// ---------------------------------------------------------------------------
// Process identification (platform-specific)
// ---------------------------------------------------------------------------

/// Get the foreground job for a given child PID.
/// Delegates to platform-specific implementation.
pub fn foreground_job(child_pid: u32) -> Option<crate::platform::ForegroundJob> {
    crate::platform::foreground_job(child_pid)
}

/// Get the foreground process group leader as a one-process job.
/// This is cheaper than collecting every process in the foreground job.
pub fn foreground_group_leader_job(
    process_group_id: u32,
) -> Option<crate::platform::ForegroundJob> {
    crate::platform::foreground_group_leader_job(process_group_id)
}

/// Get the foreground process group for a pane shell PID.
/// This is cheaper than collecting every process in the foreground job.
pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    crate::platform::foreground_process_group_id(child_pid)
}

fn normalized_process_name(process: &crate::platform::ForegroundProcess) -> String {
    let effective = process.argv0.as_deref().unwrap_or(&process.name);
    let lower_effective = effective.to_lowercase();

    if is_generic_runtime_or_shell(&lower_effective) {
        if let Some(wrapped_agent) =
            wrapped_agent_name_from_runtime_argv(&lower_effective, process.argv.as_deref())
        {
            return wrapped_agent;
        }
    }

    if identify_agent(effective).is_some() {
        return effective.to_string();
    }

    if let Some(wrapped_agent) = argv0_agent_name(process.argv.as_deref())
        .or_else(|| cmdline_argv0_agent_name(process.cmdline.as_deref().unwrap_or_default()))
    {
        return wrapped_agent;
    }

    effective.to_string()
}

fn wrapped_agent_name_from_runtime_argv(runtime: &str, argv: Option<&[String]>) -> Option<String> {
    let argv = argv?;
    let runtime = path_basename(runtime).to_lowercase();

    match runtime.as_str() {
        "node" | "bun" => script_arg_agent_name(argv, &["-e", "--eval", "-p", "--print"], &[]),
        "python" | "python3" => script_arg_agent_name(argv, &["-c"], &["-m"]),
        "sh" | "bash" | "zsh" | "fish" => script_arg_agent_name(argv, &["-c"], &[]),
        "tmux" => None,
        _ => None,
    }
}

fn script_arg_agent_name(
    argv: &[String],
    eval_flags: &[&str],
    module_flags: &[&str],
) -> Option<String> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            return args
                .next()
                .and_then(|token| agent_name_from_path_token(token));
        }

        if flag_matches(arg, eval_flags) || flag_matches(arg, module_flags) {
            return None;
        }

        if arg.starts_with('-') {
            if option_takes_value(arg) {
                let _ = args.next();
            }
            continue;
        }

        return agent_name_from_path_token(arg);
    }

    None
}

fn flag_matches(arg: &str, flags: &[&str]) -> bool {
    flags
        .iter()
        .any(|flag| arg == *flag || short_flag_payload(arg, flag) || long_flag_value(arg, flag))
}

fn short_flag_payload(arg: &str, flag: &str) -> bool {
    flag.starts_with('-')
        && !flag.starts_with("--")
        && arg.starts_with(flag)
        && arg.len() > flag.len()
}

fn long_flag_value(arg: &str, flag: &str) -> bool {
    flag.starts_with("--")
        && arg
            .strip_prefix(flag)
            .is_some_and(|rest| rest.starts_with('='))
}

fn option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-r" | "--require"
            | "--loader"
            | "--import"
            | "--experimental-loader"
            | "--inspect-port"
            | "-W"
            | "-X"
            | "-S"
            | "-L"
            | "-o"
    )
}

fn argv0_agent_name(argv: Option<&[String]>) -> Option<String> {
    agent_name_from_path_token(argv?.first()?)
}

fn cmdline_argv0_agent_name(cmdline: &str) -> Option<String> {
    agent_name_from_path_token(cmdline.split_whitespace().next()?)
}

fn agent_name_from_path_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|c| matches!(c, '"' | '\''));
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }

    agent_name_from_basename(path_basename(trimmed))
        .or_else(|| resolved_agent_name_from_path_token(trimmed))
}

fn resolved_agent_name_from_path_token(token: &str) -> Option<String> {
    let path = std::path::Path::new(token);
    if path.components().count() < 2 {
        return None;
    }

    let resolved = std::fs::canonicalize(path).ok()?;
    let basename = resolved.file_name()?.to_str()?;
    agent_name_from_basename(basename)
}

fn agent_name_from_basename(basename: &str) -> Option<String> {
    let agent = parse_agent_label(basename)?;
    Some(agent_label(agent).to_string())
}

fn path_basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
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

    fn foreground_process(
        pid: u32,
        name: &str,
        argv: &[&str],
    ) -> crate::platform::ForegroundProcess {
        crate::platform::ForegroundProcess {
            pid,
            name: name.to_string(),
            argv0: None,
            argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
            cmdline: Some(argv.join(" ")),
        }
    }

    fn temp_detection_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-detect-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn moved_agent_detection_routes_through_production_dispatch() {
        let cases = [
            (Agent::Pi, "Working...", AgentState::Working),
            (
                Agent::Claude,
                "✽ Writing…\nesc to interrupt\n──────\n❯ \n──────",
                AgentState::Working,
            ),
            (
                Agent::Codex,
                "generating code\nesc to interrupt",
                AgentState::Working,
            ),
            (Agent::Gemini, "Esc to cancel", AgentState::Working),
            (Agent::Cursor, "Ctrl+C to stop", AgentState::Working),
            (
                Agent::Antigravity,
                "⠋ Thinking through the request",
                AgentState::Working,
            ),
            (Agent::Cline, "Reading project files", AgentState::Working),
            (
                Agent::OpenCode,
                "running tool\nesc to interrupt",
                AgentState::Working,
            ),
            (Agent::GithubCopilot, "Esc cancel", AgentState::Working),
            (Agent::Kimi, "🌕", AgentState::Working),
            (Agent::Kiro, "Kiro is working", AgentState::Working),
            (Agent::Droid, "Press ESC to stop", AgentState::Working),
            (Agent::Amp, "Esc to cancel", AgentState::Working),
            (
                Agent::Grok,
                "⠋ Waiting… 1.8s\nCtrl+c:cancel Ctrl+Enter:interject",
                AgentState::Working,
            ),
            (Agent::Hermes, "msg=interrupt", AgentState::Working),
            (
                Agent::Kilo,
                "Ask · DeepSeek V4 Pro\nesc interrupt",
                AgentState::Working,
            ),
            (Agent::Qodercli, "\u{280B} Thinking...", AgentState::Working),
        ];

        for (agent, screen, expected) in cases {
            assert_eq!(
                detect_agent(Some(agent), screen).state,
                expected,
                "production dispatch for {agent:?}"
            );
        }
    }

    // ---- Agent identification ----

    #[test]
    fn identify_known_agents() {
        assert_eq!(identify_agent("pi"), Some(Agent::Pi));
        assert_eq!(identify_agent("claude"), Some(Agent::Claude));
        assert_eq!(identify_agent("claude-code"), Some(Agent::Claude));
        assert_eq!(identify_agent("codex"), Some(Agent::Codex));
        assert_eq!(identify_agent("gemini"), Some(Agent::Gemini));
        assert_eq!(identify_agent("cursor"), Some(Agent::Cursor));
        assert_eq!(identify_agent("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(identify_agent("agy"), Some(Agent::Antigravity));
        assert_eq!(identify_agent("antigravity-cli"), Some(Agent::Antigravity));
        assert_eq!(identify_agent("cline"), Some(Agent::Cline));
        assert_eq!(identify_agent("opencode"), Some(Agent::OpenCode));
        assert_eq!(identify_agent("kimi"), Some(Agent::Kimi));
        assert_eq!(identify_agent("Kimi Code"), Some(Agent::Kimi));
        assert_eq!(identify_agent("kiro"), Some(Agent::Kiro));
        assert_eq!(identify_agent("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(identify_agent("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("ghcs"), Some(Agent::GithubCopilot));
        assert_eq!(identify_agent("grok"), Some(Agent::Grok));
        assert_eq!(identify_agent("grok-build"), Some(Agent::Grok));
        assert_eq!(identify_agent("hermes"), Some(Agent::Hermes));
        assert_eq!(identify_agent("hermes-agent"), Some(Agent::Hermes));
        assert_eq!(identify_agent("kilo"), Some(Agent::Kilo));
        assert_eq!(identify_agent("kilo-code"), Some(Agent::Kilo));
    }

    #[test]
    fn parse_known_agent_labels() {
        assert_eq!(parse_agent_label("pi"), Some(Agent::Pi));
        assert_eq!(parse_agent_label("claude"), Some(Agent::Claude));
        assert_eq!(parse_agent_label("cursor-agent"), Some(Agent::Cursor));
        assert_eq!(parse_agent_label("agy"), Some(Agent::Antigravity));
        assert_eq!(parse_agent_label("antigravity"), Some(Agent::Antigravity));
        assert_eq!(parse_agent_label("copilot"), Some(Agent::GithubCopilot));
        assert_eq!(parse_agent_label("kimi-code"), Some(Agent::Kimi));
        assert_eq!(
            parse_agent_label("github-copilot"),
            Some(Agent::GithubCopilot)
        );
        assert_eq!(parse_agent_label("amp-local"), Some(Agent::Amp));
        assert_eq!(parse_agent_label("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(parse_agent_label("grok-build"), Some(Agent::Grok));
        assert_eq!(parse_agent_label("hermes-agent"), Some(Agent::Hermes));
        assert_eq!(parse_agent_label("kilo-code"), Some(Agent::Kilo));
    }

    #[test]
    fn agent_labels_use_display_names() {
        assert_eq!(agent_label(Agent::Pi), "pi");
        assert_eq!(agent_label(Agent::GithubCopilot), "copilot");
        assert_eq!(agent_label(Agent::OpenCode), "opencode");
        assert_eq!(agent_label(Agent::Antigravity), "agy");
        assert_eq!(agent_label(Agent::Kiro), "kiro");
        assert_eq!(agent_label(Agent::Grok), "grok");
        assert_eq!(agent_label(Agent::Hermes), "hermes");
        assert_eq!(agent_label(Agent::Kilo), "kilo");
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
                foreground_process(1, "node", &["node", "/path/to/bin/codex"]),
                foreground_process(2, "bash", &["bash"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_prefers_recognized_process_group_leader() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![
                foreground_process(42, "claude", &["claude"]),
                foreground_process(43, "node", &["node", "/tmp/mcp/bin/codex"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_falls_back_when_process_group_leader_is_unrecognized() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![
                foreground_process(42, "bash", &["bash"]),
                foreground_process(43, "node", &["node", "/tmp/mcp/bin/codex"]),
            ],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_nix_wrapped_codex_from_cmdline_argv0() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                ".codex-wrapped",
                &["/etc/profiles/per-user/user/bin/codex", "--model", "gpt-5"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_canonicalizes_nix_wrapped_aliases_from_cmdline_argv0() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                ".claude-code-wrapped",
                &["/nix/store/example/bin/claude-code"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Claude, "claude".to_string()))
        );
    }

    #[test]
    fn identify_agent_in_job_detects_shell_wrapped_pi() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "sh",
                &["/bin/sh", "/tmp/test-bin/pi"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Pi, "pi".to_string()))
        );
    }

    #[test]
    fn wrapped_agent_name_from_runtime_argv_ignores_plain_shell_flags() {
        assert_eq!(
            wrapped_agent_name_from_runtime_argv("bash", Some(&["bash".into(), "-lc".into()])),
            None
        );
    }

    #[test]
    fn identify_agent_in_job_ignores_python_c_argument_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "python3",
                &["python3", "-c", "import time; time.sleep(60)", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_ignores_node_eval_argument_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "node",
                &["node", "-e", "setTimeout(() => {}, 60000)", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_ignores_shell_c_argument_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "bash",
                &["bash", "-c", "sleep 60", "/tmp/codex"],
            )],
        };

        assert_eq!(identify_agent_in_job(&job), None);
    }

    #[test]
    fn identify_agent_in_job_detects_python_script_named_codex() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 123,
            processes: vec![foreground_process(
                1,
                "python3",
                &["python3", "/tmp/codex", "--model", "gpt-5"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Codex, "codex".to_string()))
        );
    }

    #[test]
    fn cmdline_argv0_agent_name_canonicalizes_known_aliases() {
        assert_eq!(
            cmdline_argv0_agent_name("/nix/store/example/bin/ghcs"),
            Some("copilot".to_string())
        );
    }

    #[test]
    fn cmdline_argv0_agent_name_requires_exact_agent_basename() {
        assert_eq!(cmdline_argv0_agent_name("/tmp/my-codex-helper"), None);
    }

    #[cfg(unix)]
    #[test]
    fn identify_agent_in_job_resolves_cursor_agent_symlink_argv0() {
        let dir = temp_detection_path("cursor-agent-symlink");
        std::fs::create_dir_all(&dir).expect("test directory should be created");
        let target = dir.join("cursor-agent");
        let link = dir.join("agent");
        std::fs::write(&target, b"#!/bin/sh\n").expect("target should be written");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should be created");

        let argv0 = link.to_string_lossy().into_owned();
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![foreground_process(
                42,
                "MainThread",
                &[&argv0, "--use-system-ca", "/tmp/index.js"],
            )],
        };

        assert_eq!(
            identify_agent_in_job(&job),
            Some((Agent::Cursor, "cursor".to_string()))
        );

        std::fs::remove_dir_all(&dir).ok();
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
    fn claude_detailed_transcript_skips_state_update() {
        let screen = "● I read the root and README.md.\n\n✻ Cogitated for 14s\n\n───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n  Showing detailed transcript · ctrl+o to toggle · ctrl+e to show all                                                                                                          verbose";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert!(detection.skip_state_update);
        assert!(!detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_detailed_transcript_collapse_skips_state_update() {
        let screen = "● Running tool\n\n───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n  Showing detailed transcript · ctrl+o to toggle · ctrl+e to collapse";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert!(detection.skip_state_update);
    }

    #[test]
    fn claude_wrapped_detailed_transcript_controls_skip_state_update() {
        let screen = "● Running tool\n\n────────────────────────\n  Showing detailed transcript · ctrl+o\n  to toggle · ctrl+e to\n  collapse";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert!(detection.skip_state_update);
    }

    #[test]
    fn claude_transcript_text_above_prompt_does_not_skip_state_update() {
        let screen = "Docs mention: Showing detailed transcript · ctrl+o to toggle · ctrl+e to show all\n\n───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n❯ \n───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert!(!detection.skip_state_update);
        assert!(detection.visible_idle);
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
    fn claude_bash_permission_modal_is_visible_blocker() {
        let screen = "● Bash(mkdir -p /tmp/herdr-claude-detector-test && for i in 1 2 3; do dd if=/dev/urandom)\n  ⎿  Waiting…\n\n────────────────────────\n Bash command\n\n   mkdir -p /tmp/herdr-claude-detector-test && ls -la /tmp/herdr-claude-detector-test\n   Create random files in temporary detector directory\n\n Contains expansion\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend · ctrl+e to explain";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn claude_cropped_bash_permission_modal_is_visible_blocker() {
        let screen = "● Bash(mkdir -p /tmp/herdr-claude-detector-test && ls -la /tmp/herdr-claude-detector-test)\n  ⎿  Waiting…\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
    }

    #[test]
    fn claude_waiting_ask_user_question_menu() {
        let screen =
            "Which approach should I take?\n❯ 1. Minimal change\n  2. Bigger refactor\n3. Chat about this\n\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";
        assert_eq!(detect_claude(screen), AgentState::Blocked);
    }

    #[test]
    fn claude_question_form_selected_top_is_visible_blocker() {
        let screen = "❯ ask again\n─────────────────────────────────────────────────────────────────────────────────────────\n←  ☐ Subject  ☐ Tone  ✔ Submit  →\n\nWhat should I ask you about?\n\n❯ 1. Today\n     Your current plan or priority.\n  2. Project\n     A codebase, feature, bug, or PR.\n  3. Preference\n     How you want me to work with you.\n  4. Random\n     A casual question with no work context.\n  5. Type something.\n─────────────────────────────────────────────────────────────────────────────────────────\n  6. Chat about this\n\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn claude_question_form_with_arrow_glyph_footer_is_visible_blocker() {
        let screen = "❯ this is a test can you use question otol to ask questiosns\n\n  Thought for 16s (ctrl+o to expand)\n─────────────────────────────────────────────────────────────────────────────────────────\n ☐ Test type\n\nWhich kind of test question should I ask you next?\n\n❯ 1. Single choice\n     Ask one multiple-choice question.\n  2. Multi choice\n     Ask one question that allows several answers.\n  3. With preview\n     Ask with side-by-side previews.\n  4. Type something.\n─────────────────────────────────────────────────────────────────────────────────────────\n  5. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to cancel";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn claude_question_form_selected_bottom_is_visible_blocker() {
        let screen = "❯ ask again\n─────────────────────────────────────────────────────────────────────────────────────────\n←  ☐ Subject  ☐ Tone  ✔ Submit  →\n\nWhat should I ask you about?\n\n  1. Today\n     Your current plan or priority.\n  2. Project\n     A codebase, feature, bug, or PR.\n  3. Preference\n     How you want me to work with you.\n  4. Random\n     A casual question with no work context.\n  5. Type something.\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ 6. Chat about this\n\nEnter to select · Tab/Arrow keys to navigate · ctrl+g to edit in Zed · Esc to cancel";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
        assert!(!detection.visible_idle);
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
    fn claude_prompt_box_is_visible_idle() {
        let screen = "Interrupted.\n─────────────\n❯ \n─────────────";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
    }

    #[test]
    fn claude_interrupted_permission_prompt_box_is_visible_idle() {
        let screen = "❯ this is a test, create some dummy files on /tmp and -rm rf them i wanna test\n    permissions\n\n  Thought for 7s (ctrl+o to expand)\n\n● Bash(tmpdir=$(mktemp -d /tmp/claude-perm-test.XXXXXX) && touch \"$tmpdir/file1.txt\"\n      \"$tmpdir/file2.log\" && mkdir \"$tmpdir/subdir\" && touch\n      \"$tmpdir/subdir/nested.txt\"…)\n  ⎿  Interrupted · What should Claude do instead?\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_declined_questions_in_scrollback_with_prompt_box_are_idle() {
        let screen = "● User declined to answer questions\n  ⎿  · What do you want help with? (Code task / PR review / Research / Claude setup)\n     · How detailed should I be? (Short / Medium / Detailed)\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~ ⊘ no git ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_old_permission_prompt_with_live_prompt_box_is_idle() {
        let screen = "● Bash(rm -rf /tmp/test)\n  ⎿  Waiting…\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n\nEsc to cancel · Tab to amend · ctrl+e to explain\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_spinner_after_interrupted_permission_is_visible_working() {
        let screen = "❯ this is a test, create some dummy files on /tmp and -rm rf them i wanna test\n    permissions\n\n  Thought for 7s (ctrl+o to expand)\n\n● Bash(tmpdir=$(mktemp -d /tmp/claude-perm-test.XXXXXX) && touch \"$tmpdir/file1.txt\"\n      \"$tmpdir/file2.log\" && mkdir \"$tmpdir/subdir\" && touch\n      \"$tmpdir/subdir/nested.txt\"…)\n  ⎿  Interrupted · What should Claude do instead?\n\n❯ test\n\n✢ Garnishing… (1s · thinking with high effort)\n  ⎿  Tip: Run claude --continue or claude --resume to resume a conversation\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_separators_without_prompt_are_not_visible_idle() {
        let screen = "Task complete.\n─────────────\nplain text\n─────────────";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn claude_spinner_above_prompt_box_is_working() {
        let screen = "✢ Imagining… (3s · thinking with high effort)\n  ⎿  Tip: Run /terminal-setup\n\n─────────────\n❯ \n─────────────\n~/project";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn claude_waiting_for_background_agent_is_working() {
        let screen = "● Done. I’ve delegated a read-only repo investigation to a subagent, and it will come back with a detailed report.\n\n✻ Waiting for 1 background agent to finish\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/llm-proxy ⎇ master ▱▱▱▱▱ 0%\n\n  ● main      ↑/↓ to select · Enter to view\n  ◯ Explore   Investigate repo and report   33s · ↓ 225 tokens";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_waiting_for_multiple_background_agents_is_working() {
        let screen = "● Done. I’ve delegated two investigations.\n\n✻ Waiting for 2 background agents to finish\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn claude_completed_background_agent_wait_in_scrollback_is_idle() {
        let screen = "❯ please create a background agent that sleeps 15 sec then say hi\n\n  Thought for 8s (ctrl+o to expand)\n\n● claude(Sleep then say hi)\n  ⎿  Backgrounded agent (↓ to manage · ctrl+o to expand)\n\n● Done. I launched a background agent that will wait 15 seconds and then reply with hi.\n\n✻ Waiting for 1 background agent to finish\n\n● Agent \"Sleep then say hi\" completed · 17s\n\n● hi\n\n✻ Brewed for 28s\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_completed_background_agent_with_prompt_box_is_idle() {
        let screen = "● Done. I’ve started a background agent.\n\n✻ Waiting for 1 background agent to finish\n\n⏺ Agent \"Sleep then say hi\" completed · 31s\n\n⏺ hi\n\n✻ Baked for 42s\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%\n\n  ● main      ↑/↓ to select · Enter to view\n  ◯ general-purpose  Sleep then say hi  31s";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn claude_zero_background_agent_wait_with_prompt_box_is_idle() {
        let screen = "✻ Waiting for 0 background agents to finish\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
    }

    #[test]
    fn claude_background_agent_wait_phrase_in_prose_is_idle() {
        let screen = "Claude mentioned: Waiting for 1 background agent to finish\n\n─────────────────────────────────────────────────────────────────────────────────────────\n❯ \n─────────────────────────────────────────────────────────────────────────────────────────\n  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%";
        let detection = detect_agent(Some(Agent::Claude), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
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
    fn codex_question_ui_is_visible_blocker() {
        let screen = "Question 1/1 (1 unanswered)\nWhat kind of code improvement do you want?\n› 1. Reduce complexity\n  2. Improve reliability\n\ntab to add notes | enter to submit answer | esc to interrupt";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
    }

    #[test]
    fn codex_bare_yes_no_hint_is_not_visible_blocker() {
        let detection = detect_agent(Some(Agent::Codex), "The docs mention [y/n] prompts.");

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn codex_replayed_transcript_weak_blocked_text_above_prompt_is_idle() {
        let screen = "Codex\nBlocked signals in src/detect/agents/codex.rs:6: confirm footer, submit answer/all, allow command, [y/n], yes (y), or generic confirmation.\n\nLikely false positives: [y/n] and generic confirmation prose still mark Blocked.\n\n• Agent thread 019e7670-ba31-7641-b6e0-545c101de8c3 is closed. Replaying saved transcript.\n\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master · gpt-5.5 default · Context 37% used";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn codex_generic_confirmation_prompt_is_not_visible_blocker() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "Earlier output asked: do you want to continue? The answer was yes.",
        );

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn non_codex_blocked_heuristics_are_not_strong_visible_blockers_by_default() {
        let detection = detect_agent(Some(Agent::Gemini), "Do you want to proceed?\n\nYes  No");

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(!detection.visible_blocker);
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
    fn codex_waiting_submit_all_multi_question_footer() {
        let screen = "Question 2/2 (1 unanswered)\nAt a high level, what is issue 249 supposed to fix?\n› 1. State arbitration (Recommended)\n  2. UI behavior\n  3. Test reliability\n  4. None of the above\n\ntab to add notes | enter to submit all | ←/→ to navigate questions | esc to interrupt";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_interrupted_prompt_is_visible_idle() {
        let screen = "■ Conversation interrupted - tell the model what to do differently. Something went\nwrong? Hit `/feedback` to report the issue.\n\n\n› Run /review on my current changes\n\n  gpt-5.5 high · ~/Projects/herdr-worktrees/issue-249-state-arbitration";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
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
    fn codex_status_line_is_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Ran git status --short\n  └ M src/detect.rs\n\n• Working (17s • esc to interrupt)\n\n\n› Implement {feature}",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_live_working_status_above_current_prompt_stays_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Ran git diff --name-status v0.6.3..HEAD\n  └ A    website/src/pages/releases/index.astro\n\n• Working (28s • esc to interrupt)\n\n\n› Run /review on my current changes\n\n  ~/Projects/herdr · master · gpt-5.5 high · Context 7% used · 5h 96% left",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_background_terminal_status_above_current_prompt_stays_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Ran just check\n  └ test output...\n\n• Working (1m 36s) · 1 background terminal running · /ps to view · /stop to close\n\n\n› Run /review on my current changes",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_reviewing_approval_request_is_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Reviewing approval request (22s • esc to interrupt)\n  └ /bin/zsh -lc 'rm -rf /tmp/codex-rm-test'\n\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_reviewing_approval_request_without_prompt_is_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Running git push --force-with-lease origin codex/ci-flake-diagnostics\n\n• Reviewing approval request (2m 53s • esc to interrupt)\n  └ /bin/zsh -lc 'git push --force-with-lease origin codex/ci-flake-diagnostics'",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_reviewing_multiple_approval_requests_is_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Reviewing 2 approval requests (0s • esc to interrupt)\n\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_booting_mcp_server_is_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Booting MCP server: codex_apps (7s • esc to interrupt)\n\n\n› ok spawn one again please\n\n  ~/Projects/herdr · master",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_stale_working_status_above_current_idle_prompt_is_idle() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "■ Conversation interrupted - tell the model what to do differently. Something went wrong?\nHit `/feedback` to report the issue.\n\n\n› `/feedback` to report the issue.\n\n\n  › Working\n\n\n  • Working (2s • esc to interrupt)\n\n\n  › Run /review on my current changes\n\n    ~/Projects/herdr · master · gpt-5.5 high · Context 7% used\n\n\n■ Conversation interrupted - tell the model what to do differently. Something went wrong?\nHit `/feedback` to report the issue.\n\n\n› Run /review on my current changes\n\n  ~/Projects/herdr · master · gpt-5.5 high · Context 7% used · 5h 95% left",
        );

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
    }

    #[test]
    fn codex_pasted_transcript_in_current_prompt_does_not_trigger_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "› so there is a problem\n\n\n\n\n  • Working (2s • esc to interrupt)\n\n\n  › Run /review on my current changes\n\n    ~/Projects/herdr · master · gpt-5.5 high · Context 0% used · 5h 94% left · weekly\n  36% …\n\n  wdyt\n\n\n\n  ~/Projects/herdr · master · gpt-5.5 high · Context 0% used · 5h 94% left · weekly 36% …",
        );

        assert_eq!(detection.state, AgentState::Idle);
        assert!(detection.visible_idle);
        assert!(!detection.visible_working);
    }

    #[test]
    fn codex_background_terminal_wait_is_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Waiting for background terminal (0s • esc to …\n  └ cargo test -p codex-core -- --exact…\n\n\n› Ask Codex to do anything",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.visible_working);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_working_header_without_interrupt_is_not_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Working (17s)\n\n› Implement {feature}",
        );

        assert_eq!(detection.state, AgentState::Working);
        assert!(!detection.visible_working);
    }

    #[test]
    fn codex_old_working_line_before_later_block_is_not_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Working (17s • esc to interrupt)\n\n• Ran git status --short\n  └ M src/detect.rs\n\n› Implement {feature}",
        );

        assert_eq!(detection.state, AgentState::Idle);
        assert!(!detection.visible_working);
        assert!(detection.visible_idle);
    }

    #[test]
    fn codex_old_background_terminal_wait_before_later_block_is_not_visible_working() {
        let detection = detect_agent(
            Some(Agent::Codex),
            "• Waiting for background terminal (0s • esc to …\n  └ cargo test -p codex-core\n\n• Ran git status --short\n  └ M src/detect.rs\n\n› Implement {feature}",
        );

        assert_eq!(detection.state, AgentState::Idle);
        assert!(!detection.visible_working);
        assert!(detection.visible_idle);
    }

    #[test]
    fn codex_queued_follow_up_keeps_active_working_status() {
        let cases = [
            "• Working (9m 24s • esc to interrupt)\n\n  • Queued follow-up inputs\n    ↳ also a new bug we can talk about later\n    alt + ↑ edit last queued message\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master",
            "• Working (9m 24s • esc to interrupt)\n\n• Queued follow-up inputs\n  ↳ also a new bug we can talk about later\n  alt + ↑ edit last queued message\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master",
            "• Working (4s • esc to interrupt)\n\n  • Messages to be submitted after next tool call (press esc to interrupt and send immediately)\n    ↳ you mean that\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master",
            "• Working (4s • esc to interrupt)\n\n• Messages to be submitted after next tool call (press esc to interrupt and send immediately)\n  ↳ you mean that\n\n› Summarize recent commits\n\n  ~/Projects/herdr · master",
        ];

        for screen in cases {
            let detection = detect_agent(Some(Agent::Codex), screen);

            assert_eq!(detection.state, AgentState::Working);
            assert!(detection.visible_working);
            assert!(!detection.visible_idle);
        }
    }

    #[test]
    fn codex_idle() {
        assert_eq!(detect_codex("❯ "), AgentState::Idle);
    }

    #[test]
    fn codex_transcript_viewer_skips_state_update() {
        let screen = "/ T R A N S C R I P T / / / / / / / / / / / / / / / /\n\n› i did thats why our latest commit is also a claude fix D\n\n• Yes, then I would release.\n──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── 100% ─\n ↑/↓ to scroll   pgup/pgdn to page   home/end to jump\n q to quit   esc to edit prev";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert!(detection.skip_state_update);
        assert!(!detection.visible_idle);
        assert!(!detection.visible_working);
        assert!(!detection.visible_blocker);
    }

    #[test]
    fn codex_wrapped_transcript_controls_skip_state_update() {
        let screen = "/ T R A N S C R I P T /\n\n› yeah go ahead\n──────────────────────────────── 100% ─\n ↑/↓ to scroll   pgup/pgdn to\n page   home/end to jump\n q to quit   esc to edit prev";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert!(detection.skip_state_update);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_esc_transcript_controls_skip_state_update() {
        let screen = "/ T R A N S C R I P T /\n\n› yeah go ahead\n──────────────────────────────── 100% ─\n ↑/↓ to scroll   pgup/pgdn to page   home/end to jump\n q to quit   esc/← to edit prev   → to edit next   enter to edit message";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert!(detection.skip_state_update);
        assert!(!detection.visible_idle);
    }

    #[test]
    fn codex_transcript_controls_above_prompt_do_not_skip_state_update() {
        let screen = "Old output:\n ↑/↓ to scroll   pgup/pgdn to page   home/end to jump\n q to quit   esc to edit prev\n\n› ";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert!(!detection.skip_state_update);
        assert!(detection.visible_idle);
    }

    #[test]
    fn codex_exit_control_without_scroll_control_does_not_skip_state_update() {
        let screen = "Some output\n\n q to quit   esc to edit prev";
        let detection = detect_agent(Some(Agent::Codex), screen);

        assert!(!detection.skip_state_update);
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
    fn cursor_working_braille_status() {
        assert_eq!(
            detect_cursor("⠠⠜ Running  5.52k tokens"),
            AgentState::Working
        );
        assert_eq!(
            detect_cursor("⠞ Working  5.62k tokens"),
            AgentState::Working
        );
        assert_eq!(
            detect_cursor("⠛ Grepping  1.2k tokens"),
            AgentState::Working
        );
    }

    #[test]
    fn cursor_blocked_command_approval() {
        let screen =
            "Waiting for approval...\nRun this command?\n→ Run (once) (y)\nSkip (esc or n)";
        assert_eq!(detect_cursor(screen), AgentState::Blocked);
    }

    #[test]
    fn cursor_running_text_with_unrelated_yes_is_not_blocked() {
        let screen = "previous answer mentioned (y)\n⠠⠜ Running  5.52k tokens";
        assert_eq!(detect_cursor(screen), AgentState::Working);
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

    // ---- Antigravity ----

    #[test]
    fn antigravity_blocked_permission_prompt() {
        let screen = "Requesting permission for: git log -n 50\nDo you want to proceed?\n> 1. Yes\n↑/↓ Navigate · tab Amend · e edit command";
        assert_eq!(detect_antigravity(screen), AgentState::Blocked);
    }

    #[test]
    fn antigravity_question_without_permission_request_stays_idle() {
        assert_eq!(
            detect_antigravity("Do you want to proceed?\n>"),
            AgentState::Idle
        );
    }

    #[test]
    fn antigravity_working_spinner() {
        assert_eq!(detect_antigravity("⡿ Working..."), AgentState::Working);
        assert_eq!(detect_antigravity("⣯ Loading..."), AgentState::Working);
        assert_eq!(detect_antigravity("⢿ Generating..."), AgentState::Working);
        assert_eq!(detect_antigravity("⣷ Running..."), AgentState::Working);
    }

    #[test]
    fn antigravity_background_task_footer_is_working() {
        let screen = "? for shortcuts     Gemini 3.5 Flash (High) · 1 task(s) · /tasks";
        assert_eq!(detect_antigravity(screen), AgentState::Working);
    }

    #[test]
    fn antigravity_background_task_footer_parses_plural_variants() {
        assert_eq!(
            detect_antigravity("Gemini 3.5 Flash (High) · 10 task(s) · /tasks"),
            AgentState::Working
        );
        assert_eq!(
            detect_antigravity("model · 2 tasks · /tasks"),
            AgentState::Working
        );
        assert_eq!(
            detect_antigravity("model · 1 task · /tasks"),
            AgentState::Working
        );
    }

    #[test]
    fn antigravity_zero_background_tasks_is_idle() {
        let screen = "Gemini 3.5 Flash (High) · 0 task(s) · /tasks";
        assert_eq!(detect_antigravity(screen), AgentState::Idle);
    }

    #[test]
    fn antigravity_task_text_outside_bottom_footer_is_idle() {
        let screen =
            "User said the footer had /tasks and 1 task(s)\nline 1\nline 2\nline 3\nline 4\nline 5";
        assert_eq!(detect_antigravity(screen), AgentState::Idle);
    }

    #[test]
    fn antigravity_idle_prompt() {
        let screen = "Antigravity CLI\n────────────────\n>\n────────────────\n? for shortcuts";
        assert_eq!(detect_antigravity(screen), AgentState::Idle);
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
    fn opencode_working_on_footer_interrupt() {
        let screen = "\
     ▣  Build · MiniMax M3 Free\n\
\n\
  ┃\n\
  ┃  Build · MiniMax M3 Free OpenCode Zen              ~/Projects/llm-proxy:master\n\
  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\n\
   ⬝⬝⬝■■■■■  esc interrupt       24.4K (12%)  ctrl+p commands    • OpenCode 1.15.13";
        assert_eq!(detect_opencode(screen), AgentState::Working);
    }

    #[test]
    fn opencode_working_on_progress_footer_without_product_text() {
        assert_eq!(
            detect_opencode("■■■■■■⬝⬝  esc interrupt"),
            AgentState::Working
        );
    }

    #[test]
    fn opencode_working_on_escape_again_footer() {
        assert_eq!(
            detect_opencode(
                "⬝⬝■■■■■■  esc again to interrupt    14.3K (7%)  ctrl+p commands    • OpenCode 1.15.13"
            ),
            AgentState::Working
        );
    }

    #[test]
    fn opencode_progress_row_alone_is_working() {
        assert_eq!(detect_opencode("■■■■■■⬝⬝"), AgentState::Working);
    }

    #[test]
    fn opencode_ctrl_p_commands_alone_is_not_working() {
        assert_eq!(
            detect_opencode("esc interrupt       24.4K (12%)  ctrl+p commands"),
            AgentState::Idle
        );
    }

    #[test]
    fn opencode_waiting_question_prompt() {
        assert_eq!(
            detect_opencode(
                "Goal   Detail   Confirm\n\
                 What do you want help with right now?\n\
                 1. Code change\n\
                 5. Type your own answer\n\
                 ⇆ tab  ↑↓ select  enter confirm  esc dismiss",
            ),
            AgentState::Blocked
        );
    }

    #[test]
    fn opencode_waiting_question_confirm_tab() {
        assert_eq!(
            detect_opencode("Goal   Detail   Confirm\nReview\n⇆ tab  enter submit  esc dismiss"),
            AgentState::Blocked
        );
    }

    #[test]
    fn opencode_idle() {
        assert_eq!(detect_opencode("> "), AgentState::Idle);
    }

    // ---- Kilo ----

    #[test]
    fn kilo_waiting_question_prompt() {
        assert_eq!(
            detect_kilo(
                "Write the following joke to ~/joke.md?\n\
                 1. Yes, write it\n\
                 2. No, thanks\n\
                 3. Type your own answer\n\
                 ↑↓ select  enter submit  esc dismiss",
            ),
            AgentState::Blocked
        );
    }

    #[test]
    fn kilo_working() {
        assert_eq!(
            detect_kilo("Ask · DeepSeek V4 Pro\nesc interrupt\n12.8K (1%) · $0.01"),
            AgentState::Working
        );
    }

    #[test]
    fn kilo_idle() {
        assert_eq!(
            detect_kilo("Ask anything... \"Fix broken tests\"\nCode · DeepSeek V4 Pro"),
            AgentState::Idle
        );
    }

    // ---- GitHub Copilot ----

    #[test]
    fn copilot_waiting_fetch_approval() {
        let content = "\
╭───────────────────────────────────────────────────────────────────╮
│ Fetch web content                                                 │
│ ───────────────────────────────────────────────────────────────── │
│ Copilot is attempting to access the following URL:                │
│                                                                   │
│ ╭───────────────────────────────────────────────────────────────╮ │
│ │ https://www.google.com/                                       │ │
│ ╰───────────────────────────────────────────────────────────────╯ │
│                                                                   │
│ Do you want to allow this access?                                 │
│                                                                   │
│ ❯ 1. Yes                                                          │
│   2. No                                                           │
│                                                                   │
│ ↑/↓ to navigate · enter to select · esc to cancel                 │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_allow_directory_access() {
        let content = "\
╭──────────────────────────────────────────────────────────────────╮
│ Allow directory access                                           │
│ ──────────────────────────────────────────────────────────────── │
│ This action may read or write the following path outside your    │
│ allowed directory list.                                          │
│                                                                   │
│ ╭──────────────────────────────────────────────────────────────╮ │
│ │ /Users/user/Dev/workspace                                │ │
│ ╰──────────────────────────────────────────────────────────────╯ │
│                                                                   │
│ Do you want to allow this?                                       │
│                                                                   │
│   1. Yes                                                         │
│ ❯ 2. Yes, and add these directories to the allowed list          │
│   3. No (Esc)                                                    │
│                                                                   │
│ ↑/↓ to navigate · enter to select · esc to cancel                │
╰──────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_directory_permission() {
        let content = "\
○ Asking user Requesting permission to access directory 'src/' for r…

╭───────────────────────────────────────────────────────────────────╮
│ Question                                                          │
│ ───────────────────────────────────────────────────────────────── │
│ Requesting permission to access directory 'src/' for reading      │
│ files. Allow?                                                     │
│                                                                   │
│ ❯ 1. Yes (Allow)                                                  │
│   2. No (Deny)                                                    │
│                                                                   │
│ ↑/↓ to select · enter to confirm · esc to cancel                  │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_action_confirmation() {
        let content = "\
○ Asking user Confirm action: 'Reset local changes in working tree' …

╭───────────────────────────────────────────────────────────────────╮
│ Question                                                          │
│ ───────────────────────────────────────────────────────────────── │
│ Confirm action: 'Reset local changes in working tree' — proceed?  │
│                                                                   │
│ ❯ 1. Yes, reset changes                                           │
│   2. No, cancel                                                   │
│                                                                   │
│ ↑/↓ to select · enter to confirm · esc to cancel                  │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_db_choice() {
        let content = "\
○ Asking user Choose a database for the project:

╭───────────────────────────────────────────────────────────────────╮
│ Question                                                          │
│ ───────────────────────────────────────────────────────────────── │
│ Choose a database for the project:                                │
│                                                                   │
│ ❯ 1. PostgreSQL (Recommended)                                     │
│   2. SQLite                                                       │
│   3. MySQL                                                        │
│   4. MongoDB                                                      │
│                                                                   │
│ ↑/↓ to select · enter to confirm · esc to cancel                  │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_freeform_input() {
        let content = "\
○ Asking user Enter the name for the new branch:

╭───────────────────────────────────────────────────────────────────╮
│ Question                                                          │
│ ───────────────────────────────────────────────────────────────── │
│ Enter the name for the new branch:                                │
│                                                                   │
│ ❯ Type your answer...                                             │
│                                                                   │
│ enter to submit · esc to cancel                                   │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_plan_review() {
        let content = "\
○ Plan ready for review - Create a 200-word inspirational poem. - Fi…

╭───────────────────────────────────────────────────────────────────╮
│ Plan Ready for Review                                             │
│ ───────────────────────────────────────────────────────────────── │
│  - Create a 200-word inspirational poem.                          │
│  - Files/changes: none (deliver poem in chat).                    │
│  - Steps: draft poem, verify ~200 words, offer up to 2 revisions. │
│  - Decision: Inspirational tone, exact length target ~200 words.  │
│                                                                   │
│ ❯ 1. Accept plan and build on default permissions (recommended)   │
│   2. Exit plan mode and I will prompt myself                      │
│   3. Suggest changes                                              │
│                                                                   │
│ ↑/↓ to navigate · enter to select · ctrl+e to show full plan ·    │
│ esc to cancel                                                     │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_waiting_enable_autopilot() {
        let content = "\
╭───────────────────────────────────────────────────────────────────╮
│ Enable autopilot mode                                             │
│ ───────────────────────────────────────────────────────────────── │
│ Autopilot mode works best with all permissions enabled. Without   │
│ them, permission requests will be auto-denied and the agent may   │
│ not complete tasks requiring file edits or shell commands.        │
│                                                                   │
│ You can also enable permissions later with /allow-all             │
│                                                                   │
│ ❯ 1. Enable all permissions (recommended)                         │
│   2. Continue with limited permissions                            │
│   3. Cancel (Esc)                                                 │
│                                                                   │
│ ↑/↓ to navigate · enter to select · esc to cancel                 │
╰───────────────────────────────────────────────────────────────────╯";
        assert_eq!(detect_github_copilot(content), AgentState::Blocked);
    }

    #[test]
    fn copilot_working_thinking_spinner() {
        assert_eq!(
            detect_github_copilot("○ Thinking esc cancel"),
            AgentState::Working
        );
        assert_eq!(
            detect_github_copilot("◎ Thinking esc cancel"),
            AgentState::Working
        );
        assert_eq!(
            detect_github_copilot("◉ Thinking esc cancel"),
            AgentState::Working
        );
    }

    // ---- Kimi ----

    #[test]
    fn kimi_blocked_approval_prompt_wins_over_spinner() {
        let screen = "⠋ Using Shell (git log --oneline -10)\n╭─ approval ─╮\nShell is requesting approval to run command:\ngit log --oneline -10\n→ [1] Approve once\n[2] Approve for this session\n[3] Reject\n[4] Reject, tell the model what to do instead\n▲/▼ select  1/2/3/4 choose  ↵ confirm";
        assert_eq!(detect_kimi(screen), AgentState::Blocked);
    }

    #[test]
    fn kimi_approval_words_without_prompt_stay_idle() {
        assert_eq!(detect_kimi("approve?"), AgentState::Idle);
        assert_eq!(detect_kimi("continue? [y/n]"), AgentState::Idle);
    }

    #[test]
    fn kimi_working_braille_thinking() {
        assert_eq!(
            detect_kimi("⠦ Thinking... <1s · 19 tokens"),
            AgentState::Working
        );
    }

    #[test]
    fn kimi_working_braille_using_tool() {
        assert_eq!(
            detect_kimi("⠹ Using Shell (git log -20 --name-status)"),
            AgentState::Working
        );
    }

    #[test]
    fn kimi_working_moon_spinner() {
        assert_eq!(detect_kimi("🌕"), AgentState::Working);
        assert_eq!(detect_kimi("🌗"), AgentState::Working);
        assert_eq!(detect_kimi("🌘"), AgentState::Working);
    }

    #[test]
    fn kimi_working_moon_spinner_above_input_box() {
        let screen = "✨ yo\n\n🌗\n\n── input ─────────────────────────";
        assert_eq!(detect_kimi(screen), AgentState::Working);
    }

    #[test]
    fn kimi_old_transcript_words_stay_idle() {
        assert_eq!(detect_kimi("thinking"), AgentState::Idle);
        assert_eq!(detect_kimi("generating code"), AgentState::Idle);
        assert_eq!(
            detect_kimi("Used Shell (git log --oneline -10)"),
            AgentState::Idle
        );
        assert_eq!(detect_kimi("some 🌕 in prose"), AgentState::Idle);
    }

    #[test]
    fn kimi_idle() {
        let screen = "Welcome to Kimi Code CLI!\n── input ─\n────────────────\nagent (Kimi-k2.6 ●)  ~/Projects/herdr";
        assert_eq!(detect_kimi(screen), AgentState::Idle);
    }

    // ---- Kiro ----

    #[test]
    fn kiro_working_on_status_bar() {
        let screen = "◕ Shell\n  esc to cancel\n● 1 MCP failure — see /mcp\n─────────────────────────────────────────────────────\nKiro · auto · ◔ 6%                                  ~\n\n Kiro is working · type to queue a message";
        assert_eq!(detect_state(Some(Agent::Kiro), screen), AgentState::Working);
    }

    #[test]
    fn kiro_working_on_tool_spinner_and_cancel_hint() {
        let screen = "◕ Shell\n  esc to cancel\n─────────────────────────────────────────────────────\nKiro · auto · ◔ 6%";
        assert_eq!(detect_state(Some(Agent::Kiro), screen), AgentState::Working);
    }

    #[test]
    fn kiro_idle_at_prompt() {
        let screen = "● 1 MCP failure — see /mcp\n──────────────────────────────────────────────────────────────────────────────────────\nKiro · auto · ◔ 6%                                                                   ~\n\n ask a question or describe a task ↵\n                                                                   /copy to clipboard";
        assert_eq!(detect_state(Some(Agent::Kiro), screen), AgentState::Idle);
    }

    #[test]
    fn kiro_blocked_on_tool_approval_prompt() {
        let screen = "↓ Shell mkdir -p /tmp/test-kiro-{a,b,c} && ls /tmp/test-kiro-*\n\n─────────────────────────────────────────────────────────────────────────────────────────\n shell requires approval\n ❯ Yes, single permission\n   Trust, always allow in this session\n   No (Tab to edit)\n─────────────────────────────────────────────────────────────────────────────────────────\n ESC to close | Tab to edit";
        assert_eq!(detect_state(Some(Agent::Kiro), screen), AgentState::Blocked);
    }

    #[test]
    fn kiro_blocked_on_subagent_tool_approval_prompt() {
        let screen = "  Please delegate to a sub-agent to search the web\n\n● Orchestrating (1 agent)\n  esc to cancel\n  ctrl+g open agent monitor\n  ● web-research kiro_default ⚠ tool approval needed\n\n ◐ Tasks · 1 done · 2 remaining                                                                                                                                              ctrl+x to expand\n──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n ⚠ 3 tool approvals pending from subagents\n ❯ (a) Approve all pending\n   (f) Approve all pending and auto-approve all future requests\n   (c) Configure individually (agent monitor)\n   (x) Exit (cancel subagents)                                 %";
        assert_eq!(detect_state(Some(Agent::Kiro), screen), AgentState::Blocked);
    }

    #[test]
    fn kiro_does_not_treat_stale_failure_spinner_as_working() {
        let screen = "● 1 MCP failure — see /mcp\n─────────────────────────────────────────────────────\nKiro · auto · ◔ 6%\n\n ask a question or describe a task ↵";
        assert_eq!(detect_state(Some(Agent::Kiro), screen), AgentState::Idle);
    }

    #[test]
    fn kiro_identified_by_process_name() {
        assert_eq!(identify_agent("kiro"), Some(Agent::Kiro));
        assert_eq!(identify_agent("kiro-cli"), Some(Agent::Kiro));
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

    // ---- Grok ----

    #[test]
    fn grok_blocked_on_permission_prompt() {
        let screen = "Show recent commit history for analysis\n\
                      git -C /home/can/Projects/herdr log --oneline --decorate -n 12\n\
                      Use ← → to choose permission whitelist scope\n\n\
                      1 (○) Always allow: git -C\n\
                      2 (●) Yes, proceed\n\
                      3 (○) No, reject (type to add feedback)\n\n\
                      1/3:select │ ←/→:scope │ Ctrl+o:yolo │ Ctrl+c:cancel";
        assert_eq!(detect_state(Some(Agent::Grok), screen), AgentState::Blocked);
    }

    #[test]
    fn grok_blocked_wins_over_spinner() {
        let screen = "⠹ Run git 30s\nYes, proceed\nNo, reject (type to add feedback)";
        assert_eq!(detect_state(Some(Agent::Grok), screen), AgentState::Blocked);
    }

    #[test]
    fn grok_working_on_waiting_spinner() {
        let screen = "⠋ Waiting… 1.8s\nCtrl+c:cancel │ Ctrl+Enter:interject";
        assert_eq!(detect_state(Some(Agent::Grok), screen), AgentState::Working);
    }

    #[test]
    fn grok_working_on_tool_spinner() {
        let screen = "⠼ Run git -C /home/can/Projects/herdr log --oneline 1.0s";
        assert_eq!(detect_state(Some(Agent::Grok), screen), AgentState::Working);
    }

    #[test]
    fn grok_idle_after_turn_completed() {
        let screen = "yo\n\nTurn completed in 1.7s.\n\n╭────╮\n│ ❯  │\n╰─ gpt-5.4 ─╯";
        assert_eq!(detect_state(Some(Agent::Grok), screen), AgentState::Idle);
    }

    // ---- Hermes ----

    #[test]
    fn hermes_identified_by_process_name() {
        assert_eq!(identify_agent("hermes"), Some(Agent::Hermes));
        assert_eq!(identify_agent("hermes-agent"), Some(Agent::Hermes));
    }

    #[test]
    fn hermes_working_on_interrupt_footer() {
        let screen = "  (⌐■_■) computing...\n\n ⚕ gpt-5.5 │ 15.5K/272K │ [█░░░░░░░░░] 6% │ 2m │ ⏱ 3s\n─────────────────────────────────────────────────────────────────────────────────────────\n⚕ ❯ msg=interrupt · /queue · /bg · /steer · Ctrl+C cancel";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Working
        );
    }

    #[test]
    fn hermes_idle_ignores_stale_initializing_agent_text() {
        let screen = "● say exactly READY and stop\nInitializing agent...\n\n╭─ ⚕ Hermes ────────────────────────────────────────────────────────────────────────────╮\n    READY\n╰───────────────────────────────────────────────────────────────────────────────────────╯\n ⚕ gpt-5.5 │ 15.5K/272K │ [█░░░░░░░░░] 6% │ 15s │ ⏲ 2s\n─────────────────────────────────────────────────────────────────────────────────────────\n❯";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    #[test]
    fn hermes_blocked_on_dangerous_command_prompt() {
        let screen = "╭────────────────────────────────────────────────────────────╮\n│ ⚠️  Dangerous Command                                      │\n│ mkdir -p /tmp/herdr-hermes-block-test/subdir && touch      │\n│ ❯ 1. Allow once                                            │\n│   2. Allow for this session                                │\n│   3. Add to permanent allowlist                            │\n│   4. Deny                                                  │\n│   5. Show full command                                     │\n╰────────────────────────────────────────────────────────────╯\n  ↑/↓ to select, Enter to confirm\n⚠ ❯";
        assert_eq!(
            detect_state(Some(Agent::Hermes), screen),
            AgentState::Blocked
        );
    }

    #[test]
    fn hermes_idle_at_prompt_after_response() {
        let screen = "╭─ ⚕ Hermes ────────────────────────────────────────────────────────────────────────────╮\n    READY\n╰───────────────────────────────────────────────────────────────────────────────────────╯\n ⚕ gpt-5.5 │ 15.5K/272K │ [█░░░░░░░░░] 6% │ 15s │ ⏲ 2s\n─────────────────────────────────────────────────────────────────────────────────────────\n❯\n─────────────────────────────────────────────────────────────────────────────────────────";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    #[test]
    fn hermes_denied_message_is_idle() {
        let screen = "╭─ ⚕ Hermes ────────────────────────────────────────────────────────────────────────────╮\n    Command was blocked/denied by the safety layer. I did not retry.\n╰───────────────────────────────────────────────────────────────────────────────────────╯\n ⚕ gpt-5.5 │ 15.4K/272K │ [█░░░░░░░░░] 6% │ 2m │ ⏲ 11s\n─────────────────────────────────────────────────────────────────────────────────────────\n❯";
        assert_eq!(detect_state(Some(Agent::Hermes), screen), AgentState::Idle);
    }

    // ---- Qodercli ----

    #[test]
    fn qodercli_identified_by_process_name() {
        assert_eq!(identify_agent("qodercli"), Some(Agent::Qodercli));
        assert_eq!(identify_agent("qoderclicn"), Some(Agent::Qodercli));
        assert_eq!(identify_agent("qoder"), Some(Agent::Qodercli));
        assert_eq!(identify_agent("qodercn"), Some(Agent::Qodercli));
    }

    #[test]
    fn qodercli_blocked_on_confirmation() {
        assert_eq!(
            detect_qodercli("Waiting for user confirmation..."),
            AgentState::Blocked,
        );
    }

    #[test]
    fn qodercli_working_on_spinner() {
        assert_eq!(detect_qodercli("\u{280B} Thinking..."), AgentState::Working);
    }

    #[test]
    fn qodercli_idle_on_prompt() {
        assert_eq!(detect_qodercli("> "), AgentState::Idle);
    }

    #[test]
    fn qodercli_idle_when_only_stale_braille_glyph_in_scrollback() {
        // A single stray braille character in a previous output line must not
        // flip the pane to Working — only an actual spinner *row* should.
        let screen = "\
agent finished a previous task.\n\
\u{280B}\n\
> \n";
        assert_eq!(detect_qodercli(screen), AgentState::Idle);
    }

    #[test]
    fn qodercli_working_on_full_spinner_row() {
        // Real spinner row: braille glyph + space + alphabetic phrase.
        let screen = "\u{280B} Thinking...\n";
        assert_eq!(detect_qodercli(screen), AgentState::Working);
    }

    #[test]
    fn qodercli_working_on_esc_to_cancel_hint() {
        // The "(esc to cancel, …)" suffix is qodercli's explicit working
        // marker. It must trigger Working even if a hook icon replaced the
        // spinner glyph in this frame.
        let screen = "Thinking... (esc to cancel, 5s)\n";
        assert_eq!(detect_qodercli(screen), AgentState::Working);
    }

    #[test]
    fn qodercli_idle_when_text_mentions_working_in_prose() {
        // The previous heuristic treated the bare word "working" as Working,
        // which produced false positives for narrative output (commits, logs,
        // Markdown). The pane should remain Idle until a real working signal
        // appears.
        let screen = "\
fix: keep working set warm across reloads\n\
\n\
> \n";
        assert_eq!(detect_qodercli(screen), AgentState::Idle);
    }

    #[test]
    fn qodercli_idle_override_wins_over_spinner_row() {
        // While the user is holding Ctrl+C, qodercli flashes a "press again"
        // banner over the prompt. The pane is effectively idle there even if
        // a stale spinner row is still in the buffer.
        let screen = "\
\u{280B} Thinking...\n\
Press Ctrl+C again to exit.\n";
        assert_eq!(detect_qodercli(screen), AgentState::Idle);
    }

    #[test]
    fn qodercli_idle_override_wins_over_esc_rewind() {
        let screen = "Press Esc again to rewind.\n";
        assert_eq!(detect_qodercli(screen), AgentState::Idle);
    }

    #[test]
    fn qodercli_blocked_on_permission_required_dialog() {
        // qodercli renders this dialog when a tool call needs user approval.
        let screen = "\
Permission Required\n\
Caller: test\n\
Command: mkdir -p /root/example\n\
Allow once or always?\n\
  \u{276F} 1. Allow Once - allow `mkdir` for one\n\
    2. Always allow `mkdir` for future sessions\n\
    3. Reject and tell qodercli something\n";
        assert_eq!(detect_qodercli(screen), AgentState::Blocked);
    }

    #[test]
    fn qodercli_blocked_on_permission_required_alone() {
        // Even when the prompt copy gets truncated by the viewport, the title
        // alone should be enough to flip the pane to blocked.
        assert_eq!(detect_qodercli("Permission Required"), AgentState::Blocked,);
    }

    #[test]
    fn qodercli_blocked_on_askuser_enter_response_placeholder() {
        // qodercli's ask-user tool renders an input box with this placeholder
        // when waiting for the user to type a response.
        let screen = "\
What kind of project are you working on?\n\
> \n\
  Enter your response\n";
        assert_eq!(detect_qodercli(screen), AgentState::Blocked);
    }

    #[test]
    fn qodercli_blocked_on_askuser_review_tab() {
        // The multi-question/multi-select review tab heading is unique to the
        // ask-user dialog and means the agent is parked waiting on user input.
        let screen = "\
Review your answers:\n\
\n\
Project type \u{2192} Web app\n\
Stack        \u{2192} (not answered)\n";
        assert_eq!(detect_qodercli(screen), AgentState::Blocked);
    }

    #[test]
    fn qodercli_blocked_on_interactive_shell_waiting() {
        // When qodercli spawns an interactive shell, the loading row turns
        // into a "Shell awaiting input" hint until the user takes focus.
        let screen = "! Shell awaiting input (Tab to focus)\n";
        assert_eq!(detect_qodercli(screen), AgentState::Blocked);
    }

    #[test]
    fn qodercli_blocked_on_askuser_single_choice_dialog() {
        // Single-select ask-user has no "Enter your response" placeholder and
        // no "Review your answers:" heading. The BaseTabDialog title
        // "Asking User" is the only stable signal across every ask-user form.
        let screen = "\
Asking User\n\
\n\
Which framework should we use?\n\
  React\n\
  Vue\n\
  Svelte\n";
        assert_eq!(detect_qodercli(screen), AgentState::Blocked);
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
        assert!(has_cursor_spinner("⠠⠜ Running  5.52k tokens"));
        assert!(has_cursor_spinner("⠞ Working  5.62k tokens"));
        assert!(has_cursor_spinner("⠛ Grepping  1.2k tokens"));
        assert!(has_cursor_spinner("⠛ Analyzing  1.2k tokens"));
    }

    #[test]
    fn cursor_spinner_not_false_positive() {
        assert!(!has_cursor_spinner("normal text"));
        assert!(!has_cursor_spinner("some ⬡ in middle"));
        assert!(!has_cursor_spinner("⠛ Read notes"));
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
