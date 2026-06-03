pub(super) mod amp;
pub(super) mod antigravity;
pub(super) mod claude_code;
pub(super) mod cline;
pub(super) mod codex;
pub(super) mod cursor;
pub(super) mod droid;
pub(super) mod gemini;
pub(super) mod github_copilot;
pub(super) mod grok;
pub(super) mod hermes;
pub(super) mod kilo;
pub(super) mod kimi;
pub(super) mod kiro;
pub(super) mod opencode;
pub(super) mod pi;
pub(super) mod qodercli;

use super::{Agent, AgentDetection, AgentState};

pub(super) fn detect(agent: Agent, screen_content: &str) -> AgentDetection {
    let state = match agent {
        Agent::Pi => pi::detect(screen_content),
        Agent::Claude => claude_code::detect(screen_content),
        Agent::Codex => codex::detect(screen_content),
        Agent::Gemini => gemini::detect(screen_content),
        Agent::Cursor => cursor::detect(screen_content),
        Agent::Antigravity => antigravity::detect(screen_content),
        Agent::Cline => cline::detect(screen_content),
        Agent::OpenCode => opencode::detect(screen_content),
        Agent::GithubCopilot => github_copilot::detect(screen_content),
        Agent::Kimi => kimi::detect(screen_content),
        Agent::Kiro => kiro::detect(screen_content),
        Agent::Droid => droid::detect(screen_content),
        Agent::Amp => amp::detect(screen_content),
        Agent::Grok => grok::detect(screen_content),
        Agent::Hermes => hermes::detect(screen_content),
        Agent::Kilo => kilo::detect(screen_content),
        Agent::Qodercli => qodercli::detect(screen_content),
    };

    let skip_state_update = should_skip_state_update(agent, screen_content);
    if skip_state_update {
        return AgentDetection {
            state,
            skip_state_update: true,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
        };
    }

    AgentDetection {
        state,
        skip_state_update: false,
        visible_blocker: has_visible_blocker(agent, screen_content, state),
        visible_idle: has_visible_idle(agent, screen_content, state),
        visible_working: has_visible_working(agent, screen_content, state),
    }
}

pub(super) fn should_skip_state_update(agent: Agent, content: &str) -> bool {
    match agent {
        Agent::Claude => claude_code::is_transcript_viewer(content),
        Agent::Codex => codex::is_transcript_viewer(content),
        _ => false,
    }
}

fn has_visible_blocker(agent: Agent, content: &str, state: AgentState) -> bool {
    if state != AgentState::Blocked {
        return false;
    }

    match agent {
        // Strong visible blockers are opt-in because this flag can override
        // hook authority. Plain blocked heuristics remain valid fallback state,
        // but they must not become hook overrides unless the current UI chrome
        // is known to be structural and live.
        Agent::Claude => claude_code::has_visible_blocker(content),
        Agent::Codex => codex::has_visible_blocker(content),
        _ => false,
    }
}

fn has_visible_idle(agent: Agent, content: &str, state: AgentState) -> bool {
    if state != AgentState::Idle {
        return false;
    }

    match agent {
        Agent::Claude => claude_code::has_prompt_box(content),
        Agent::Codex => codex::has_prompt(content),
        _ => false,
    }
}

fn has_visible_working(agent: Agent, content: &str, state: AgentState) -> bool {
    if state != AgentState::Working {
        return false;
    }

    match agent {
        Agent::Claude => claude_code::has_working_chrome(content),
        Agent::Codex => codex::has_visible_working(content),
        _ => false,
    }
}
