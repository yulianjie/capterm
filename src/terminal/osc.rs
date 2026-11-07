use std::mem;

#[derive(Default)]
pub(super) struct Scanner {
    state: State,
    payload: Vec<u8>,
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
    Ignore,
    IgnoreEscape,
}

pub(super) enum Marker {
    Directory(String),
    Notification(String),
    Prompt,
    PromptEnd,
    Output,
    Done(Option<i32>),
}

impl Scanner {
    // Offsets are immediately after each recognized OSC. The caller passes all
    // bytes through to the VT parser and reads the cursor at these boundaries.
    pub(super) fn scan(&mut self, bytes: &[u8]) -> Vec<(usize, Marker)> {
        use State::*;
        let mut events = Vec::new();
        for (offset, &byte) in bytes.iter().enumerate() {
            self.state = match (mem::take(&mut self.state), byte) {
                (Ground, 0x1b) => Escape,
                (Escape, b']') => {
                    self.payload.clear();
                    Osc
                }
                (Escape, b'P' | b'X' | b'^' | b'_') => Ignore,
                (Escape, 0x1b) => Escape,
                (Osc, 0x1b) => OscEscape,
                (Osc, 0x07) | (OscEscape, b'\\') => {
                    if let Some(event) = parse(&self.payload) {
                        events.push((offset + 1, event));
                    }
                    Ground
                }
                (Osc, 0x18 | 0x1a) => Ground,
                (Osc, _) if self.payload.len() < 8192 => {
                    self.payload.push(byte);
                    Osc
                }
                (Osc, _) | (Ignore, _) if byte == 0x1b => IgnoreEscape,
                (Osc, _) | (Ignore, _) if matches!(byte, 0x07 | 0x18 | 0x1a) => Ground,
                (Osc, _) | (Ignore, _) => Ignore,
                (IgnoreEscape, b'\\') => Ground,
                (IgnoreEscape, 0x1b) => IgnoreEscape,
                (IgnoreEscape, _) => Ignore,
                (OscEscape, b']') => {
                    self.payload.clear();
                    Osc
                }
                (OscEscape, 0x1b) => Escape,
                _ => Ground,
            };
        }
        events
    }
}

fn parse(payload: &[u8]) -> Option<Marker> {
    let payload = std::str::from_utf8(payload).ok()?;
    let (code, value) = payload.split_once(';')?;
    match code {
        "7" => {
            let (_, path) = value.strip_prefix("file://")?.split_once('/')?;
            let bytes = path.as_bytes();
            let mut decoded = vec![b'/'];
            let mut index = 0;
            while let Some(&byte) = bytes.get(index) {
                if byte == b'%' {
                    let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
                    decoded.push(u8::from_str_radix(hex, 16).ok()?);
                    index += 3;
                } else {
                    decoded.push(byte);
                    index += 1;
                }
            }
            let path = String::from_utf8(decoded).ok()?;
            (!path.chars().any(char::is_control)).then_some(Marker::Directory(path))
        }
        "9" => Some(Marker::Notification(
            value.chars().filter(|c| !c.is_control()).collect(),
        )),
        "777" => {
            let rest = value.strip_prefix("notify;")?;
            let message = rest.replace(';', " · ");
            Some(Marker::Notification(
                message.chars().filter(|c| !c.is_control()).collect(),
            ))
        }
        "133" => {
            let mut fields = value.split(';');
            match fields.next()? {
                "A" => Some(Marker::Prompt),
                "B" => Some(Marker::PromptEnd),
                "C" => Some(Marker::Output),
                "D" => Some(Marker::Done(
                    fields.next().and_then(|status| status.parse().ok()),
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_fragmented_osc_at_the_correct_cursor_boundary() {
        let mut scanner = Scanner::default();
        assert!(scanner.scan(b"text\x1b]7;file://host/tmp/a%").is_empty());
        let markers = scanner.scan(b"20b\x1b\\\x1b]133;D;1\x07prompt");
        assert!(
            matches!(&markers[..], [(5, Marker::Directory(path)), (15, Marker::Done(Some(1)))] if path == "/tmp/a b")
        );
    }
}
