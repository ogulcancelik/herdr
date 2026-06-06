#[derive(Debug, Clone, Default)]
pub(crate) struct KittyKeyboardTracker {
    pending: Vec<u8>,
    stack: Vec<u16>,
    flags: u16,
}

impl KittyKeyboardTracker {
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        let combined;
        let bytes = if self.pending.is_empty() {
            bytes
        } else {
            combined = self
                .pending
                .iter()
                .copied()
                .chain(bytes.iter().copied())
                .collect::<Vec<_>>();
            self.pending.clear();
            &combined
        };
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0x1b {
                index += 1;
                continue;
            }
            if index + 1 >= bytes.len() {
                self.store_pending(&bytes[index..]);
                break;
            }
            if bytes[index + 1] != b'[' {
                index += 1;
                continue;
            }

            let mut end = index + 2;
            while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                end += 1;
            }
            if end >= bytes.len() {
                self.store_pending(&bytes[index..]);
                break;
            }

            if bytes[end] == b'u' {
                self.observe_csi_u(&bytes[index + 2..end]);
            }
            index = end + 1;
        }
    }

    fn store_pending(&mut self, bytes: &[u8]) {
        self.pending.clear();
        if bytes.len() <= 64 {
            self.pending.extend_from_slice(bytes);
        }
    }

    fn observe_csi_u(&mut self, params: &[u8]) {
        let Some((&kind, rest)) = params.split_first() else {
            return;
        };
        match kind {
            b'>' => {
                let flags = parse_kitty_keyboard_flags(rest);
                self.stack.push(self.flags);
                self.flags = flags;
            }
            b'=' => {
                self.flags = parse_kitty_keyboard_flags(rest);
            }
            b'<' => {
                let count = parse_kitty_keyboard_flags(rest).max(1);
                for _ in 0..count {
                    self.flags = self.stack.pop().unwrap_or(0);
                }
            }
            _ => {}
        }
    }

    #[cfg(unix)]
    pub(crate) fn replay_ansi(&self) -> Option<String> {
        if self.stack.is_empty() {
            return (self.flags != 0).then(|| format!("\x1b[={}u", self.flags));
        }

        let mut ansi = String::new();
        let baseline = self.stack[0];
        if baseline != 0 {
            ansi.push_str(&format!("\x1b[={baseline}u"));
        }
        for flags in self.stack.iter().skip(1).copied().chain([self.flags]) {
            ansi.push_str(&format!("\x1b[>{flags}u"));
        }
        (!ansi.is_empty()).then_some(ansi)
    }
}

fn parse_kitty_keyboard_flags(bytes: &[u8]) -> u16 {
    let first_param = bytes.split(|byte| *byte == b';').next().unwrap_or_default();
    std::str::from_utf8(first_param)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_split_csi_sequences() {
        let mut tracker = KittyKeyboardTracker::default();

        tracker.observe(b"\x1b[>1u\x1b[>5");
        tracker.observe(b"u\x1b[<");
        tracker.observe(b"u");

        assert_eq!(tracker.flags, 1);
        assert_eq!(tracker.stack, vec![0]);
    }
}
