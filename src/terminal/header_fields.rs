//! Session-promoted pane header fields ("chips").
//!
//! Sessions know things herdr cannot derive: dev containers they started,
//! long-task progress, ports, model names. `pane.set_header_field` lets the
//! session register small `key value` chips onto its own pane's header (and
//! the navigation surfaces that ride `PaneDetail`). Fields are ephemeral by
//! design — a restored session's containers are unknown — so they are never
//! persisted into snapshots. An optional TTL auto-expires chips whose
//! producer stopped updating (progress that stops updating shouldn't lie);
//! expiry rides the same scheduled tick that already expires agent metadata.

use std::time::{Duration, Instant};

use super::TerminalState;

/// Hard cap on chips per pane (abuse/leak guard).
pub const MAX_HEADER_FIELDS: usize = 6;
/// Hard cap on a chip key, in characters.
pub const MAX_HEADER_FIELD_KEY_CHARS: usize = 16;
/// Hard cap on a chip value, in characters.
pub const MAX_HEADER_FIELD_VALUE_CHARS: usize = 48;

/// One promoted header field: `key value`, optionally expiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderField {
    pub key: String,
    pub value: String,
    pub expires_at: Option<Instant>,
}

/// Why a set-field request was rejected. Surfaced as an RPC error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFieldError {
    EmptyKey,
    EmptyValue,
    KeyTooLong,
    ValueTooLong,
    TooManyFields,
}

impl std::fmt::Display for HeaderFieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyKey => write!(f, "field key must not be empty"),
            Self::EmptyValue => write!(f, "field value must not be empty"),
            Self::KeyTooLong => write!(
                f,
                "field key exceeds {MAX_HEADER_FIELD_KEY_CHARS} characters"
            ),
            Self::ValueTooLong => write!(
                f,
                "field value exceeds {MAX_HEADER_FIELD_VALUE_CHARS} characters"
            ),
            Self::TooManyFields => {
                write!(f, "pane already has {MAX_HEADER_FIELDS} header fields")
            }
        }
    }
}

/// Normalize and validate a key/value pair: trim, strip control characters,
/// reject empties and over-cap lengths (over-cap is an error, not a silent
/// truncation — the producer should know its chip was refused).
pub fn validate_header_field(key: &str, value: &str) -> Result<(String, String), HeaderFieldError> {
    let key = normalize_field_text(key);
    let value = normalize_field_text(value);
    if key.is_empty() {
        return Err(HeaderFieldError::EmptyKey);
    }
    if value.is_empty() {
        return Err(HeaderFieldError::EmptyValue);
    }
    if key.chars().count() > MAX_HEADER_FIELD_KEY_CHARS {
        return Err(HeaderFieldError::KeyTooLong);
    }
    if value.chars().count() > MAX_HEADER_FIELD_VALUE_CHARS {
        return Err(HeaderFieldError::ValueTooLong);
    }
    Ok((key, value))
}

fn normalize_field_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Compact `key value · key value` summary for the narrow navigation
/// surfaces (navigator pane lists, mobile pane lists); values are
/// middle-truncated to `value_cap` columns. `None` when there are no fields.
pub fn compact_header_fields(fields: &[(String, String)], value_cap: usize) -> Option<String> {
    if fields.is_empty() {
        return None;
    }
    Some(
        fields
            .iter()
            .map(|(key, value)| format!("{key} {}", middle_truncate_chars(value, value_cap)))
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

/// Middle-truncate to `width` characters, keeping start and end
/// ("abcdef", 5 -> "ab…ef").
pub fn middle_truncate_chars(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "\u{2026}".to_string();
    }
    let keep = width - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('\u{2026}');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

impl TerminalState {
    /// Set (or update in place, preserving insertion order) a promoted
    /// header field. Validation and the per-pane cap are both enforced here;
    /// the RPC handler pre-checks the same rules so it can surface a
    /// synchronous error to the caller.
    pub fn set_header_field(
        &mut self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), HeaderFieldError> {
        self.set_header_field_at(key, value, ttl, Instant::now())
    }

    pub fn set_header_field_at(
        &mut self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
        now: Instant,
    ) -> Result<(), HeaderFieldError> {
        let (key, value) = validate_header_field(key, value)?;
        self.expire_header_fields_at(now);
        let expires_at = ttl.map(|ttl| now + ttl);
        if let Some(field) = self.header_fields.iter_mut().find(|f| f.key == key) {
            field.value = value;
            field.expires_at = expires_at;
            return Ok(());
        }
        if self.header_fields.len() >= MAX_HEADER_FIELDS {
            return Err(HeaderFieldError::TooManyFields);
        }
        self.header_fields.push(HeaderField {
            key,
            value,
            expires_at,
        });
        Ok(())
    }

    /// Whether a set of `key` would fit under the per-pane cap right now.
    /// Updating an existing key never hits the cap.
    pub fn has_header_field_capacity(&self, key: &str, now: Instant) -> bool {
        let active = self
            .header_fields
            .iter()
            .filter(|field| !header_field_expired(field, now));
        let mut count = 0usize;
        for field in active {
            if field.key == key {
                return true;
            }
            count += 1;
        }
        count < MAX_HEADER_FIELDS
    }

    /// Remove a promoted field. Idempotent; returns whether it existed.
    pub fn clear_header_field(&mut self, key: &str) -> bool {
        let key = normalize_field_text(key);
        let before = self.header_fields.len();
        self.header_fields.retain(|field| field.key != key);
        self.header_fields.len() != before
    }

    /// Non-expired fields in insertion order, as renderable pairs. TTL is
    /// enforced at read time too, so renders never show a stale chip even
    /// before the scheduled expiry tick fires.
    pub fn active_header_fields(&self) -> Vec<(String, String)> {
        self.active_header_fields_at(Instant::now())
    }

    pub fn active_header_fields_at(&self, now: Instant) -> Vec<(String, String)> {
        self.header_fields
            .iter()
            .filter(|field| !header_field_expired(field, now))
            .map(|field| (field.key.clone(), field.value.clone()))
            .collect()
    }

    /// Earliest TTL deadline across this pane's fields. Feeds the shared
    /// scheduled-task deadline alongside agent-metadata expiry.
    pub fn next_header_field_expiry(&self) -> Option<Instant> {
        self.header_fields
            .iter()
            .filter_map(|field| field.expires_at)
            .min()
    }

    /// Drop fields whose TTL has elapsed; returns whether anything changed.
    pub fn expire_header_fields_at(&mut self, now: Instant) -> bool {
        let before = self.header_fields.len();
        self.header_fields
            .retain(|field| !header_field_expired(field, now));
        self.header_fields.len() != before
    }
}

fn header_field_expired(field: &HeaderField, now: Instant) -> bool {
    field.expires_at.is_some_and(|deadline| deadline <= now)
}

#[cfg(test)]
mod tests {
    use super::super::TerminalId;
    use super::*;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    #[test]
    fn set_and_clear_round_trip_preserves_insertion_order() {
        let mut terminal = test_terminal();
        terminal.set_header_field("build", "73%", None).unwrap();
        terminal.set_header_field("pg", "up", None).unwrap();
        assert_eq!(
            terminal.active_header_fields(),
            vec![
                ("build".to_string(), "73%".to_string()),
                ("pg".to_string(), "up".to_string()),
            ]
        );

        // Updating an existing key keeps its slot.
        terminal.set_header_field("build", "74%", None).unwrap();
        assert_eq!(
            terminal.active_header_fields(),
            vec![
                ("build".to_string(), "74%".to_string()),
                ("pg".to_string(), "up".to_string()),
            ]
        );

        assert!(terminal.clear_header_field("build"));
        assert!(!terminal.clear_header_field("build"));
        assert_eq!(
            terminal.active_header_fields(),
            vec![("pg".to_string(), "up".to_string())]
        );
    }

    #[test]
    fn validation_rejects_empty_and_over_cap_fields() {
        assert_eq!(
            validate_header_field("  ", "x"),
            Err(HeaderFieldError::EmptyKey)
        );
        assert_eq!(
            validate_header_field("k", " \t "),
            Err(HeaderFieldError::EmptyValue)
        );
        assert_eq!(
            validate_header_field(&"k".repeat(MAX_HEADER_FIELD_KEY_CHARS + 1), "v"),
            Err(HeaderFieldError::KeyTooLong)
        );
        assert_eq!(
            validate_header_field("k", &"v".repeat(MAX_HEADER_FIELD_VALUE_CHARS + 1)),
            Err(HeaderFieldError::ValueTooLong)
        );
        // Control characters are stripped, surrounding whitespace trimmed.
        assert_eq!(
            validate_header_field(" bu\u{7}ild ", " 73%\u{1b} "),
            Ok(("build".to_string(), "73%".to_string()))
        );
    }

    #[test]
    fn cap_rejects_a_seventh_field_but_allows_updates() {
        let mut terminal = test_terminal();
        for i in 0..MAX_HEADER_FIELDS {
            terminal
                .set_header_field(&format!("k{i}"), "v", None)
                .unwrap();
        }
        assert_eq!(
            terminal.set_header_field("k6", "v", None),
            Err(HeaderFieldError::TooManyFields)
        );
        assert!(!terminal.has_header_field_capacity("k6", Instant::now()));
        // Existing keys always have capacity.
        assert!(terminal.has_header_field_capacity("k0", Instant::now()));
        terminal.set_header_field("k0", "v2", None).unwrap();
    }

    #[test]
    fn ttl_expires_fields_at_read_time_and_via_the_expiry_sweep() {
        let mut terminal = test_terminal();
        let now = Instant::now();
        terminal
            .set_header_field_at("build", "73%", Some(Duration::from_secs(5)), now)
            .unwrap();
        terminal.set_header_field_at("pg", "up", None, now).unwrap();

        assert_eq!(
            terminal.next_header_field_expiry(),
            Some(now + Duration::from_secs(5))
        );
        assert_eq!(terminal.active_header_fields_at(now).len(), 2);

        // Read-time filtering hides the expired chip before any sweep runs.
        let later = now + Duration::from_secs(6);
        assert_eq!(
            terminal.active_header_fields_at(later),
            vec![("pg".to_string(), "up".to_string())]
        );

        assert!(terminal.expire_header_fields_at(later));
        assert!(!terminal.expire_header_fields_at(later));
        assert_eq!(terminal.next_header_field_expiry(), None);
        // An expired slot frees capacity for new keys.
        assert!(terminal.has_header_field_capacity("fresh", later));
    }

    #[test]
    fn compact_header_fields_formats_and_truncates() {
        assert_eq!(compact_header_fields(&[], 16), None);
        let fields = vec![
            ("build".to_string(), "73%".to_string()),
            ("model".to_string(), "claude-fable-5-20260120".to_string()),
        ];
        assert_eq!(
            compact_header_fields(&fields, 10).as_deref(),
            Some("build 73% · model claud…0120")
        );
    }
}
