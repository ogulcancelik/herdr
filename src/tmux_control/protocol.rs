//! Tmux control mode event parsing.
//!
//! Parses the line-oriented event stream produced by `tmux -C`.
//! Events are prefixed with `%` and followed by space-separated fields.
//!
//! Reference: tmux manual CONTROL MODE section.

use std::fmt;

/// A parsed event from tmux control mode.
#[derive(Debug, Clone, PartialEq)]
pub enum TmuxEvent {
    /// Pane produced output: `%output %pane-id data...`
    Output { pane: String, data: String },
    /// Active pane changed: `%pane-focus-changed %session %window %pane`
    PaneFocusChanged {
        session: String,
        window: String,
        pane: String,
    },
    /// Window added: `%window-add %session %window`
    WindowAdd { session: String, window: String },
    /// Window closed: `%window-close %session %window`
    WindowClose { session: String, window: String },
    /// Session changed: `%session-changed %session`
    SessionChanged { session: String },
    /// Session created: `%session-created %session`
    SessionCreated { session: String },
    /// Session closed: `%session-closed %session`
    SessionClosed { session: String },
    /// Output paused (buffer full): `%pause %session %window %pane`
    Pause {
        session: String,
        window: String,
        pane: String,
    },
    /// Control mode ready (after initial command): `%begin %cmd-identifier`
    Begin { cmd_identifier: String },
    /// Control mode output line: `%output %cmd-identifier data`
    CmdOutput {
        cmd_identifier: String,
        data: String,
    },
    /// Control mode end: `%end %cmd-identifier %exit-value`
    End {
        cmd_identifier: String,
        exit_value: String,
    },
    /// Unknown event (preserved for forward compatibility).
    Unknown(String),
}

/// Error from parsing a tmux control mode line.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub line: String,
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to parse tmux control mode line: {}: {}",
            self.line, self.reason
        )
    }
}

impl std::error::Error for ParseError {}

/// Parse a single line from tmux control mode output.
///
/// Returns `None` for empty lines or lines that don't start with `%`.
pub fn parse_line(line: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let trimmed = line.trim_end();

    // Empty lines are ignored (used as separators in control mode).
    if trimmed.is_empty() {
        return Ok(None);
    }

    // Control mode lines start with `%`.
    let Some(rest) = trimmed.strip_prefix('%') else {
        return Ok(None);
    };

    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
    let event_name = parts[0];
    let args = parts.get(1).unwrap_or(&"");

    match event_name {
        "output" => parse_output(args),
        "pane-focus-changed" => parse_pane_focus_changed(args),
        "window-add" => parse_window_add(args),
        "window-close" => parse_window_close(args),
        "session-changed" => parse_session_changed(args),
        "session-created" => parse_session_created(args),
        "session-closed" => parse_session_closed(args),
        "pause" => parse_pause(args),
        "begin" => Ok(Some(TmuxEvent::Begin {
            cmd_identifier: args.trim().to_string(),
        })),
        "end" => parse_end(args),
        _ if event_name.starts_with("output") => parse_output(args),
        _ => Ok(Some(TmuxEvent::Unknown(trimmed.to_string()))),
    }
}

/// Parse all lines from a control mode output chunk.
pub fn parse_lines(text: &str) -> Vec<TmuxEvent> {
    text.lines()
        .filter_map(|line| match parse_line(line) {
            Ok(Some(event)) => Some(event),
            _ => None,
        })
        .collect()
}

fn parse_output(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    // Format: %pane-id data...
    // Pane IDs start with `%` in tmux.
    let Some(rest) = args.strip_prefix('%') else {
        return Err(ParseError {
            line: args.to_string(),
            reason: "output event missing pane ID".to_string(),
        });
    };

    // Split on first space — pane ID is one token, rest is data.
    let mut parts = rest.splitn(2, ' ');
    let pane_id = parts.next().unwrap_or("").to_string();
    let data = parts.next().unwrap_or("").to_string();

    Ok(Some(TmuxEvent::Output {
        pane: pane_id,
        data,
    }))
}

fn parse_pane_focus_changed(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(ParseError {
            line: args.to_string(),
            reason: "pane-focus-changed needs session, window, pane".to_string(),
        });
    }
    Ok(Some(TmuxEvent::PaneFocusChanged {
        session: parts[0].to_string(),
        window: parts[1].to_string(),
        pane: parts[2].to_string(),
    }))
}

fn parse_window_add(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(ParseError {
            line: args.to_string(),
            reason: "window-add needs session, window".to_string(),
        });
    }
    Ok(Some(TmuxEvent::WindowAdd {
        session: parts[0].to_string(),
        window: parts[1].to_string(),
    }))
}

fn parse_window_close(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(ParseError {
            line: args.to_string(),
            reason: "window-close needs session, window".to_string(),
        });
    }
    Ok(Some(TmuxEvent::WindowClose {
        session: parts[0].to_string(),
        window: parts[1].to_string(),
    }))
}

fn parse_session_changed(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let session = args.trim().to_string();
    if session.is_empty() {
        return Err(ParseError {
            line: args.to_string(),
            reason: "session-changed needs session name".to_string(),
        });
    }
    Ok(Some(TmuxEvent::SessionChanged { session }))
}

fn parse_session_created(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let session = args.trim().to_string();
    if session.is_empty() {
        return Err(ParseError {
            line: args.to_string(),
            reason: "session-created needs session name".to_string(),
        });
    }
    Ok(Some(TmuxEvent::SessionCreated { session }))
}

fn parse_session_closed(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let session = args.trim().to_string();
    if session.is_empty() {
        return Err(ParseError {
            line: args.to_string(),
            reason: "session-closed needs session name".to_string(),
        });
    }
    Ok(Some(TmuxEvent::SessionClosed { session }))
}

fn parse_pause(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(ParseError {
            line: args.to_string(),
            reason: "pause needs session, window, pane".to_string(),
        });
    }
    Ok(Some(TmuxEvent::Pause {
        session: parts[0].to_string(),
        window: parts[1].to_string(),
        pane: parts[2].to_string(),
    }))
}

fn parse_end(args: &str) -> Result<Option<TmuxEvent>, ParseError> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(ParseError {
            line: args.to_string(),
            reason: "end needs cmd-identifier, exit-value".to_string(),
        });
    }
    Ok(Some(TmuxEvent::End {
        cmd_identifier: parts[0].to_string(),
        exit_value: parts[1].to_string(),
    }))
}

/// Strip ANSI escape sequences from control mode output data.
pub fn strip_ansi(data: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
    let re_osc = regex::Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap();
    let re_c1 = regex::Regex::new(r"\x1b[@-_\x80-\x9f]").unwrap();

    let text = re.replace_all(data, "");
    let text = re_osc.replace_all(&text, "");
    let text = re_c1.replace_all(&text, "");
    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_event() {
        let event = parse_line("%output %42 hello world").unwrap().unwrap();
        match event {
            TmuxEvent::Output { pane, data } => {
                assert_eq!(pane, "42");
                assert_eq!(data, "hello world");
            }
            _ => panic!("expected Output event"),
        }
    }

    #[test]
    fn parse_output_with_ansi() {
        let event = parse_line(r#"%output %7 \x1b[1mBold\x1b[0m"#)
            .unwrap()
            .unwrap();
        match event {
            TmuxEvent::Output { pane, data } => {
                assert_eq!(pane, "7");
                assert!(data.contains("\\x1b[1m"));
            }
            _ => panic!("expected Output event"),
        }
    }

    #[test]
    fn parse_pane_focus_changed() {
        let event = parse_line("%pane-focus-changed mysession 1 %3")
            .unwrap()
            .unwrap();
        match event {
            TmuxEvent::PaneFocusChanged {
                session,
                window,
                pane,
            } => {
                assert_eq!(session, "mysession");
                assert_eq!(window, "1");
                assert_eq!(pane, "%3");
            }
            _ => panic!("expected PaneFocusChanged"),
        }
    }

    #[test]
    fn parse_window_add() {
        let event = parse_line("%window-add mysession 2").unwrap().unwrap();
        match event {
            TmuxEvent::WindowAdd { session, window } => {
                assert_eq!(session, "mysession");
                assert_eq!(window, "2");
            }
            _ => panic!("expected WindowAdd"),
        }
    }

    #[test]
    fn parse_session_events() {
        let created = parse_line("%session-created new-sess").unwrap().unwrap();
        let closed = parse_line("%session-closed old-sess").unwrap().unwrap();
        let changed = parse_line("%session-changed active-sess").unwrap().unwrap();

        assert_eq!(
            created,
            TmuxEvent::SessionCreated {
                session: "new-sess".into()
            }
        );
        assert_eq!(
            closed,
            TmuxEvent::SessionClosed {
                session: "old-sess".into()
            }
        );
        assert_eq!(
            changed,
            TmuxEvent::SessionChanged {
                session: "active-sess".into()
            }
        );
    }

    #[test]
    fn parse_empty_line_returns_none() {
        assert!(parse_line("").unwrap().is_none());
        assert!(parse_line("   ").unwrap().is_none());
    }

    #[test]
    fn parse_unknown_event() {
        let event = parse_line("%future-event some args").unwrap().unwrap();
        assert_eq!(event, TmuxEvent::Unknown("%future-event some args".into()));
    }

    #[test]
    fn parse_lines_batch() {
        let text = "%session-created main\n%window-add main 1\n%output %5 $ _\n";
        let events = parse_lines(text);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], TmuxEvent::SessionCreated { .. }));
        assert!(matches!(events[1], TmuxEvent::WindowAdd { .. }));
        assert!(matches!(events[2], TmuxEvent::Output { .. }));
    }

    #[test]
    fn strip_ansi_codes() {
        let input = "\x1b[1mBold\x1b[0m Normal";
        assert_eq!(strip_ansi(input), "Bold Normal");
    }
}
