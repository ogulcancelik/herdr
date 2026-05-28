//! [`FormatResolver`] implementations that bind the format engine to concrete
//! data. [`StatusContext`] is the herdr binding; it carries pre-resolved values
//! so the engine stays decoupled from herdr types.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::expand::FormatResolver;

/// `#(command)` results are cached for this long so the status bar (re-rendered
/// at ~12fps) spawns each unique command at most ~once per second.
const SHELL_TTL: Duration = Duration::from_millis(1000);

fn shell_cache() -> &'static Mutex<HashMap<String, (Instant, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Run `command` via `sh -c`, caching stdout for [`SHELL_TTL`]. Best-effort:
/// failures yield an empty string. Only reached when `tmux_compat` is on, and
/// the user owns the commands, so a brief synchronous run is acceptable.
fn run_shell_cached(command: &str) -> String {
    if let Ok(cache) = shell_cache().lock() {
        if let Some((at, val)) = cache.get(command) {
            if at.elapsed() < SHELL_TTL {
                return val.clone();
            }
        }
    }
    let val = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if let Ok(mut cache) = shell_cache().lock() {
        cache.insert(command.to_string(), (Instant::now(), val.clone()));
    }
    val
}

/// Pre-resolved variable values for one rendered status line.
///
/// Built by the render layer from herdr state. Native variables:
/// - `session_name` — the owning workspace's display name
/// - `pane_title` — agent/effective title (OSC title under `tmux_compat`)
/// - `agent_state` — `working` | `idle` | `blocked` | `unknown`
/// - `agent_done` — `1` when the agent finished while unseen, else `0`
/// - `window_activity` — epoch seconds of last output (Layer B; `None` otherwise)
///
/// User options (`@name`) fall through to `user_options`.
#[derive(Debug, Default, Clone)]
pub struct StatusContext {
    pub session_name: String,
    pub pane_title: String,
    pub agent_state: String,
    pub agent_done: bool,
    pub window_activity: Option<String>,
    pub user_options: HashMap<String, String>,
    /// When true, `#(shell)` expansion runs commands (Layer B). Off by default
    /// so the clean default never spawns subprocesses.
    pub allow_shell: bool,
}

impl FormatResolver for StatusContext {
    fn var(&self, name: &str) -> Option<Cow<'_, str>> {
        let v = match name {
            "session_name" => return Some(Cow::Borrowed(self.session_name.as_str())),
            "pane_title" => return Some(Cow::Borrowed(self.pane_title.as_str())),
            "agent_state" => return Some(Cow::Borrowed(self.agent_state.as_str())),
            "agent_done" => return Some(Cow::Borrowed(if self.agent_done { "1" } else { "0" })),
            "window_activity" => return self.window_activity.as_deref().map(Cow::Borrowed),
            other => other,
        };
        // User options are stored without the leading `@`.
        let key = v.strip_prefix('@').unwrap_or(v);
        self.user_options.get(key).map(|s| Cow::Borrowed(s.as_str()))
    }

    fn shell(&self, command: &str) -> Option<String> {
        self.allow_shell.then(|| run_shell_cached(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::expand::expand_str;

    #[test]
    fn shell_disabled_by_default() {
        let ctx = StatusContext {
            allow_shell: false,
            ..Default::default()
        };
        // No subprocess runs; the `#(...)` yields nothing.
        assert_eq!(expand_str("[#(echo hi)]", &ctx), "[]");
    }

    #[test]
    fn shell_runs_when_allowed() {
        let ctx = StatusContext {
            allow_shell: true,
            ..Default::default()
        };
        assert_eq!(expand_str("[#(echo hi)]", &ctx), "[hi]");
    }

    #[test]
    fn native_vars_resolve() {
        let ctx = StatusContext {
            session_name: "ws".into(),
            agent_state: "working".into(),
            agent_done: true,
            window_activity: Some("1000".into()),
            ..Default::default()
        };
        assert_eq!(expand_str("#{session_name}", &ctx), "ws");
        assert_eq!(expand_str("#{agent_state}", &ctx), "working");
        assert_eq!(expand_str("#{agent_done}", &ctx), "1");
        assert_eq!(expand_str("#{window_activity}", &ctx), "1000");
    }
}
