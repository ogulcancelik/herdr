//! tmux format-string expansion: `#{...}` expressions, `#(...)` shell commands,
//! `#[...]` style directives, single-char aliases (`#S`, `#T`, ...), and `##`.
//!
//! This is a clean-room Rust reimplementation of the subset of tmux's `format.c`
//! that the status bar needs (see `docs/plans/tmux-status-bar.md`). It is a pure
//! function of the input string and a [`FormatResolver`]; it has no herdr
//! coupling, so it is unit- and parity-testable in isolation.
//!
//! Known divergences from tmux (documented, acceptable for the status bar):
//! - `#{...}` brace matching is structural; a `}` *inside* a substitution pattern
//!   (e.g. `s/a}b/x/`) closes the expression early. Patterns needing literal `}`
//!   are unsupported.
//! - Numeric comparison falls back to byte-string ordering when operands aren't
//!   both parseable as `f64`.

use std::borrow::Cow;

use ratatui::style::Style;

use super::style::apply_style_spec;

/// Source of variable and user-option values, plus optional `#(shell)` output.
///
/// `var("session_name")` returns built-in/native variables; `var("@foo")` returns
/// the *raw* (unexpanded) user-option value — `#{E:@foo}` re-expands it.
pub trait FormatResolver {
    fn var(&self, name: &str) -> Option<Cow<'_, str>>;
    /// Expand a `#(command)` to its (already-cached) stdout. Default: unsupported.
    fn shell(&self, command: &str) -> Option<String> {
        let _ = command;
        None
    }
}

/// Render a format string into styled `(text, Style)` segments, starting from
/// `base`. `#[...]` directives switch the active style for following text.
pub fn render_segments(input: &str, base: Style, r: &dyn FormatResolver) -> Vec<(String, Style)> {
    let mut segs: Vec<(String, Style)> = Vec::new();
    let mut cur = base;
    let mut buf = String::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                segs.push((std::mem::take(&mut buf), cur));
            }
        };
    }

    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'#' => {
                    buf.push('#');
                    i += 2;
                    continue;
                }
                b'{' => {
                    if let Some(close) = matching_close(input, i) {
                        buf.push_str(&eval_inner(&input[i + 2..close], r));
                        i = close + 1;
                        continue;
                    }
                }
                b'(' => {
                    if let Some(close) = matching_close(input, i) {
                        let cmd = expand_str(&input[i + 2..close], r);
                        if let Some(out) = r.shell(&cmd) {
                            buf.push_str(out.trim_end_matches('\n'));
                        }
                        i = close + 1;
                        continue;
                    }
                }
                b'[' => {
                    if let Some(close) = matching_close(input, i) {
                        let spec = expand_str(&input[i + 2..close], r);
                        flush!();
                        cur = apply_style_spec(&spec, base, cur);
                        i = close + 1;
                        continue;
                    }
                }
                other => {
                    if let Some(name) = alias(other) {
                        buf.push_str(&var_lookup(name, r));
                        i += 2;
                        continue;
                    }
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }
    flush!();
    segs
}

/// Expand a format string to a plain `String` (style directives consumed but not
/// applied). Used for nested contexts: conditions, comparison operands, the
/// values fed to modifiers, and `#(shell)` command text.
pub fn expand_str(input: &str, r: &dyn FormatResolver) -> String {
    let mut out = String::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'#' => {
                    out.push('#');
                    i += 2;
                    continue;
                }
                b'{' => {
                    if let Some(close) = matching_close(input, i) {
                        out.push_str(&eval_inner(&input[i + 2..close], r));
                        i = close + 1;
                        continue;
                    }
                }
                b'(' => {
                    if let Some(close) = matching_close(input, i) {
                        let cmd = expand_str(&input[i + 2..close], r);
                        if let Some(o) = r.shell(&cmd) {
                            out.push_str(o.trim_end_matches('\n'));
                        }
                        i = close + 1;
                        continue;
                    }
                }
                b'[' => {
                    if let Some(close) = matching_close(input, i) {
                        i = close + 1;
                        continue;
                    }
                }
                other => {
                    if let Some(name) = alias(other) {
                        out.push_str(&var_lookup(name, r));
                        i += 2;
                        continue;
                    }
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Evaluate the contents of a single `#{...}` (without the braces).
fn eval_inner(content: &str, r: &dyn FormatResolver) -> String {
    // Conditional: #{?condition,true,false}
    if let Some(body) = content.strip_prefix('?') {
        let (cond, rest) = split_first(body, b',');
        let (t, f) = match rest {
            Some(rest) => split_first(rest, b','),
            None => ("", None),
        };
        // tmux treats a bare condition name as a variable reference
        // (e.g. `#{?client_prefix,...}`), while `#{...}` conditions expand.
        let cond_val = eval_or_var(cond, r);
        return if is_true(&cond_val) {
            expand_str(t, r)
        } else {
            expand_str(f.unwrap_or(""), r)
        };
    }

    // Comparison / logical operators: #{OP:a,b}
    for op in ["||", "&&", "==", "!=", "<=", ">=", "<", ">"] {
        if let Some(arg) = content
            .strip_prefix(op)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            let (a, b) = split_first(arg, b',');
            let av = expand_str(a, r);
            let bv = expand_str(b.unwrap_or(""), r);
            return bool_str(compare(op, &av, &bv));
        }
    }

    // Substitution: #{s/pattern/replacement/flags:arg}
    if content.starts_with('s') {
        if let Some(&d) = content.as_bytes().get(1) {
            if !d.is_ascii_alphanumeric() && d != b':' {
                if let Some(res) = eval_substitution(content, r) {
                    return res;
                }
            }
        }
    }

    // Expand modifier: #{E:expr} — expand the value of expr a second time.
    if let Some(inner) = content.strip_prefix("E:") {
        let once = eval_inner(inner, r);
        return expand_str(&once, r);
    }

    // Truncation: #{=N:arg} (truncate to N columns).
    if let Some(rest) = content.strip_prefix('=') {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Some(arg) = rest[digits.len()..].strip_prefix(':') {
                let n: usize = digits.parse().unwrap_or(0);
                return truncate_cols(&eval_or_var(arg, r), n);
            }
        }
    }

    // Simple string modifiers.
    if let Some(arg) = content.strip_prefix("b:") {
        return basename(&eval_or_var(arg, r));
    }
    if let Some(arg) = content.strip_prefix("d:") {
        return dirname(&eval_or_var(arg, r));
    }
    if let Some(arg) = content.strip_prefix("q:") {
        return eval_or_var(arg, r);
    }

    // Bare variable / user option.
    var_lookup(content, r)
}

/// A modifier argument may be a nested format or a bare variable name.
fn eval_or_var(arg: &str, r: &dyn FormatResolver) -> String {
    if arg.contains("#{") || arg.contains("#(") || arg.starts_with('#') {
        expand_str(arg, r)
    } else {
        var_lookup(arg, r)
    }
}

fn var_lookup(name: &str, r: &dyn FormatResolver) -> String {
    r.var(name).map(|c| c.into_owned()).unwrap_or_default()
}

fn eval_substitution(content: &str, r: &dyn FormatResolver) -> Option<String> {
    let b = content.as_bytes();
    let delim = b[1];
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 2;
    while i < b.len() && parts.len() < 2 {
        let c = b[i];
        if c == b'\\' && i + 1 < b.len() {
            // Preserve escapes for the regex (e.g. `\/` -> escaped delimiter).
            cur.push('\\');
            let ch = content[i + 1..].chars().next().unwrap();
            cur.push(ch);
            i += 1 + ch.len_utf8();
            continue;
        }
        if c == delim {
            parts.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        let ch = content[i..].chars().next().unwrap();
        cur.push(ch);
        i += ch.len_utf8();
    }
    if parts.len() < 2 {
        return None;
    }
    let pattern = parts[0].clone();
    let replacement = parts[1].clone();
    let rest = &content[i..];
    let (flags, arg) = match rest.find(':') {
        Some(p) => (&rest[..p], &rest[p + 1..]),
        None => return None,
    };
    let mut global = false;
    let mut icase = false;
    for f in flags.chars() {
        match f {
            'g' => global = true,
            'i' => icase = true,
            _ => {}
        }
    }
    let target = eval_or_var(arg, r);
    let pat = if icase {
        format!("(?i){pattern}")
    } else {
        pattern
    };
    let re = regex::Regex::new(&pat).ok()?;
    let rep = translate_replacement(&replacement);
    let out = if global {
        re.replace_all(&target, rep.as_str()).into_owned()
    } else {
        re.replace(&target, rep.as_str()).into_owned()
    };
    Some(out)
}

/// Translate tmux `\N` backreferences to Rust-regex `${N}` and escape literal `$`.
fn translate_replacement(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            out.push_str("${");
            out.push(b[i + 1] as char);
            out.push('}');
            i += 2;
            continue;
        }
        if c == b'$' {
            out.push_str("$$");
            i += 1;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// tmux truthiness: non-empty and not `"0"`.
fn is_true(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

fn bool_str(b: bool) -> String {
    if b { "1" } else { "0" }.to_string()
}

fn compare(op: &str, a: &str, b: &str) -> bool {
    match op {
        "==" => a == b,
        "!=" => a != b,
        "&&" => is_true(a) && is_true(b),
        "||" => is_true(a) || is_true(b),
        _ => {
            if let (Ok(x), Ok(y)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
                match op {
                    "<" => x < y,
                    ">" => x > y,
                    "<=" => x <= y,
                    ">=" => x >= y,
                    _ => false,
                }
            } else {
                match op {
                    "<" => a < b,
                    ">" => a > b,
                    "<=" => a <= b,
                    ">=" => a >= b,
                    _ => false,
                }
            }
        }
    }
}

fn alias(b: u8) -> Option<&'static str> {
    Some(match b {
        b'S' => "session_name",
        b'T' => "pane_title",
        b'W' => "window_name",
        b'H' => "host",
        b'h' => "host_short",
        b'I' => "window_index",
        b'P' => "pane_index",
        b'D' => "pane_id",
        b'F' => "window_flags",
        _ => return None,
    })
}

fn truncate_cols(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut width = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w > max {
            break;
        }
        width += w;
        out.push(ch);
    }
    out
}

fn basename(s: &str) -> String {
    s.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(s)
        .to_string()
}

fn dirname(s: &str) -> String {
    let trimmed = s.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(p) => trimmed[..p].to_string(),
        None => ".".to_string(),
    }
}

/// Find the byte index of the closer matching the `#{`/`#(`/`#[` whose `#` is at
/// `open_at`. Brace tracking keys off `#`-prefixed openers, so bare `[`/`]`/`(`
/// inside regex patterns are not counted.
fn matching_close(s: &str, open_at: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let first = match bytes.get(open_at + 1)? {
        b'{' => b'}',
        b'(' => b')',
        b'[' => b']',
        _ => return None,
    };
    let mut stack = vec![first];
    let mut i = open_at + 2;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'#' => {
                    i += 2;
                    continue;
                }
                b'{' => {
                    stack.push(b'}');
                    i += 2;
                    continue;
                }
                b'(' => {
                    stack.push(b')');
                    i += 2;
                    continue;
                }
                b'[' => {
                    stack.push(b']');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if c == *stack.last().unwrap() {
            stack.pop();
            if stack.is_empty() {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split `s` at the first top-level (depth-0) occurrence of `delim`.
fn split_first(s: &str, delim: u8) -> (&str, Option<&str>) {
    let bytes = s.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'#' => {
                    i += 2;
                    continue;
                }
                b'{' => {
                    stack.push(b'}');
                    i += 2;
                    continue;
                }
                b'(' => {
                    stack.push(b')');
                    i += 2;
                    continue;
                }
                b'[' => {
                    stack.push(b']');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(&top) = stack.last() {
            if c == top {
                stack.pop();
                i += 1;
                continue;
            }
        } else if c == delim {
            return (&s[..i], Some(&s[i + 1..]));
        }
        i += 1;
    }
    (s, None)
}
