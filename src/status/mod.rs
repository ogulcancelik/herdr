//! tmux-compatible status bar: a clean-room subset of tmux's format-expansion
//! engine plus status-line composition, rendered into ratatui.
//!
//! Layered design (see `docs/plans/tmux-status-bar.md`):
//! - **Layer A (this module + bindings + render):** the engine and renderer, bound
//!   to herdr state that already exists. Self-contained and unit-testable.
//! - **Layer B (gated `tmux_compat`):** OSC-title capture and a `window_activity`
//!   timestamp that touch shared hot paths; provided elsewhere, opt-in.

pub mod bar;
pub mod expand;
pub mod resolver;
pub mod style;

pub use bar::{compose, StatusSpec};
pub use resolver::StatusContext;

#[cfg(test)]
mod tests {
    use super::expand::{expand_str, render_segments, FormatResolver};
    use super::*;
    use std::borrow::Cow;
    use std::collections::HashMap;

    /// Fixed-map resolver for engine tests (no herdr coupling).
    struct StubResolver {
        vars: HashMap<String, String>,
        shells: HashMap<String, String>,
    }

    impl StubResolver {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
                shells: HashMap::new(),
            }
        }
        fn with(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.to_string(), v.to_string());
            self
        }
    }

    impl FormatResolver for StubResolver {
        fn var(&self, name: &str) -> Option<Cow<'_, str>> {
            self.vars.get(name).map(|s| Cow::Borrowed(s.as_str()))
        }
        fn shell(&self, command: &str) -> Option<String> {
            self.shells.get(command).cloned()
        }
    }

    fn expand(input: &str, r: &dyn FormatResolver) -> String {
        expand_str(input, r)
    }

    #[test]
    fn plain_variable() {
        let r = StubResolver::new().with("session_name", "work");
        assert_eq!(expand("#{session_name}", &r), "work");
        assert_eq!(expand("#S", &r), "work");
    }

    #[test]
    fn literal_hash_escape() {
        let r = StubResolver::new();
        assert_eq!(expand("a##b", &r), "a#b");
    }

    #[test]
    fn conditional_truthy() {
        let r = StubResolver::new().with("x", "1");
        assert_eq!(expand("#{?x,yes,no}", &r), "yes");
        let r0 = StubResolver::new().with("x", "0");
        assert_eq!(expand("#{?x,yes,no}", &r0), "no");
        let re = StubResolver::new().with("x", "");
        assert_eq!(expand("#{?x,yes,no}", &re), "no");
    }

    #[test]
    fn comparison_eq() {
        let r = StubResolver::new().with("a", "3");
        assert_eq!(expand("#{==:#{a},3}", &r), "1");
        assert_eq!(expand("#{==:#{a},4}", &r), "0");
        assert_eq!(expand("#{?#{==:#{a},3},match,nope}", &r), "match");
    }

    #[test]
    fn substitution_strip_nondigits() {
        let r = StubResolver::new().with("session_name", "herdr7srv");
        assert_eq!(expand("#{s/[^0-9]//g:session_name}", &r), "7");
    }

    #[test]
    fn substitution_no_digits_empty() {
        let r = StubResolver::new().with("session_name", "herdrsrv");
        assert_eq!(expand("#{s/[^0-9]//g:session_name}", &r), "");
    }

    #[test]
    fn expand_modifier_reexpands_user_option() {
        // @sc holds a format string; #{E:@sc} expands it against the same vars.
        let sc = "#{?#{==:#{s/[^0-9]//g:session_name},1},colour167,#{?#{==:#{s/[^0-9]//g:session_name},3},colour226,colour244}}";
        let r = StubResolver::new()
            .with("@sc", sc)
            .with("session_name", "3");
        assert_eq!(expand("#{E:@sc}", &r), "colour226");

        let r1 = StubResolver::new().with("@sc", sc).with("session_name", "1");
        assert_eq!(expand("#{E:@sc}", &r1), "colour167");

        let rg = StubResolver::new()
            .with("@sc", sc)
            .with("session_name", "herdrsrv");
        assert_eq!(expand("#{E:@sc}", &rg), "colour244");
    }

    /// Parity cases verified byte-for-byte against `tmux 3.5a` via
    /// `tmux display-message -p` on 2026-05-28 (see docs/plans/tmux-status-bar.md,
    /// finding 0.4). `@name` stands in for tmux's built-in `session_name` so the
    /// inputs are controllable user options on both sides.
    #[test]
    fn parity_with_tmux_3_5a() {
        let sc = "#{?#{==:#{s/[^0-9]//g:@name},1},colour167,#{?#{==:#{s/[^0-9]//g:@name},2},colour208,#{?#{==:#{s/[^0-9]//g:@name},3},colour226,colour244}}}";
        let cases: &[(&str, &[(&str, &str)], &str)] = &[
            ("#{s/[^0-9]//g:@name}", &[("@name", "herdr7srv")], "7"),
            ("#{s/[^0-9]//g:@name}", &[("@name", "herdrsrv")], ""),
            (
                "#{?#{==:#{s/[^0-9]//g:@name},1},RED,OTHER}",
                &[("@name", "ws1")],
                "RED",
            ),
            ("#{E:@sc}", &[("@sc", sc), ("@name", "1")], "colour167"),
            ("#{E:@sc}", &[("@sc", sc), ("@name", "2")], "colour208"),
            ("#{E:@sc}", &[("@sc", sc), ("@name", "3")], "colour226"),
            ("#{E:@sc}", &[("@sc", sc), ("@name", "herdrsrv")], "colour244"),
            ("#{=5:@title}", &[("@title", "hello world")], "hello"),
            ("#{?@flag,yes,no}", &[("@flag", "0")], "no"),
            ("#{?@flag,yes,no}", &[("@flag", "1")], "yes"),
            ("a##b", &[], "a#b"),
        ];
        for (fmt, vars, expected) in cases {
            let mut r = StubResolver::new();
            for (k, v) in *vars {
                r = r.with(k, v);
            }
            assert_eq!(&expand(fmt, &r), expected, "tmux parity: {fmt:?} {vars:?}");
        }
    }

    #[test]
    fn truncate_modifier() {
        let r = StubResolver::new().with("t", "hello world");
        assert_eq!(expand("#{=5:#{t}}", &r), "hello");
    }

    #[test]
    fn shell_expansion() {
        let mut r = StubResolver::new().with("window_activity", "1000");
        r.shells.insert("idle.sh 1000".to_string(), "3m\n".to_string());
        assert_eq!(expand("#(idle.sh #{window_activity})", &r), "3m");
    }

    #[test]
    fn style_segments_switch_style() {
        use ratatui::style::{Color, Modifier};
        let r = StubResolver::new()
            .with("@sc", "colour208")
            .with("session_name", "work");
        let segs = render_segments(
            "#[fg=colour16,bg=#{E:@sc},bold] #{session_name} ",
            ratatui::style::Style::default(),
            &r,
        );
        // First non-empty styled segment carries the directive's style.
        let styled = segs.iter().find(|(t, _)| t.contains("work")).unwrap();
        assert_eq!(styled.1.fg, Some(Color::Indexed(16)));
        assert_eq!(styled.1.bg, Some(Color::Indexed(208)));
        assert!(styled.1.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn compose_fills_width_and_flushes_right() {
        let r = StubResolver::new()
            .with("pane_title", "claude")
            .with("session_name", "1");
        let spec = StatusSpec {
            style: "bg=colour244,fg=colour16",
            left: "",
            right: "#[fg=colour16,bold] #{session_name} ",
            window: "#[fg=colour16,bold] #{pane_title} ",
            left_length: 0,
            right_length: 12,
        };
        let line = compose(&spec, 40, &r);
        assert_eq!(line.width(), 40, "status line must fill the width");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with(" claude "), "title flush-left: {text:?}");
        assert!(text.trim_end().ends_with(" 1"), "session flush-right: {text:?}");
    }
}
