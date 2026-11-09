use crate::config::ShiftEnterPreference;
use alacritty_terminal::term::TermMode;
use gpui_kit::{Keystroke, Modifiers, MouseButton};

pub(super) fn requires_paste_confirmation(
    text: &str,
    bracketed: bool,
    confirm_multiline: bool,
) -> bool {
    confirm_multiline && !bracketed && text.contains(['\r', '\n'])
}

pub(super) fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        // A pasted escape must not close the bracketed-paste delimiter.
        let text = text.replace(['\x1b', '\0'], "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

fn modifier_parameter(m: Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control)
}

pub(super) fn is_line_enter(
    key: &Keystroke,
    mode: TermMode,
    shift_enter: ShiftEnterPreference,
) -> bool {
    let m = key.modifiers;
    key.key == "enter"
        && !m.control
        && !m.alt
        && !m.platform
        && (!m.shift
            || (shift_enter == ShiftEnterPreference::Enter
                && !mode.contains(TermMode::DISAMBIGUATE_ESC_CODES)))
}

pub(super) fn key_bytes(
    key: &Keystroke,
    mode: TermMode,
    shift_enter: ShiftEnterPreference,
) -> Option<Vec<u8>> {
    let m = key.modifiers;
    if m.platform {
        return None;
    }

    let key_name = key.key.as_str();
    let parameter = modifier_parameter(m);
    let disambiguate = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    if disambiguate && let Some(code) = disambiguated_key(key) {
        return Some(if parameter == 1 {
            format!("\x1b[{code}u").into_bytes()
        } else {
            format!("\x1b[{code};{parameter}u").into_bytes()
        });
    }
    let mut sequence = if let Some(final_byte) = match key_name {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" if !disambiguate => Some('R'),
        "f4" => Some('S'),
        _ => None,
    } {
        if parameter != 1 {
            return Some(format!("\x1b[1;{parameter}{final_byte}").into_bytes());
        }
        let prefix = if !disambiguate
            && (mode.contains(TermMode::APP_CURSOR) || matches!(final_byte, 'P'..='S'))
        {
            b'O'
        } else {
            b'['
        };
        return Some(vec![0x1b, prefix, final_byte as u8]);
    } else if let Some(code) = match key_name {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f3" if disambiguate => Some(13),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    } {
        return Some(if parameter == 1 {
            format!("\x1b[{code}~").into_bytes()
        } else {
            format!("\x1b[{code};{parameter}~").into_bytes()
        });
    } else {
        match key_name {
            "enter"
                if m.shift
                    && !m.control
                    && !m.alt
                    && shift_enter == ShiftEnterPreference::LineFeed =>
            {
                return Some(vec![b'\n']);
            }
            "enter" if !is_line_enter(key, mode, shift_enter) => return Some(b"\x1b\r".to_vec()),
            "enter" if mode.contains(TermMode::LINE_FEED_NEW_LINE) => b"\r\n".to_vec(),
            "enter" => vec![b'\r'],
            "tab" if m.shift => return Some(b"\x1b[Z".to_vec()),
            "tab" => vec![b'\t'],
            "escape" => vec![0x1b],
            "backspace" if m.control => vec![0x08],
            "backspace" => vec![0x7f],
            // On Windows AltGr is presented as Control+Alt with a composed character.
            _ if m.control && m.alt && key.key_char.is_some() => return None,
            _ if m.control => vec![control_byte(key_name, key.key_char.as_deref())?],
            _ if m.alt && key_name.len() == 1 => {
                let key_char = key.key_char.as_deref().unwrap_or(key_name);
                // Option can produce its own Unicode text on macOS.
                if cfg!(target_os = "macos") && !key_char.is_ascii() {
                    return None;
                }
                key_char.as_bytes().to_vec()
            }
            // Let GPUI deliver ordinary text and IME commits to the input handler.
            _ => return None,
        }
    };
    if m.alt {
        sequence.insert(0, 0x1b);
    }
    Some(sequence)
}

fn disambiguated_key(key: &Keystroke) -> Option<u32> {
    let m = key.modifiers;
    let modified = m.shift || m.control || m.alt;
    match key.key.as_str() {
        "escape" => Some(27),
        "enter" if modified => Some(13),
        "tab" if modified => Some(9),
        "backspace" if modified => Some(127),
        "space" if m.control || m.alt => Some(32),
        // Preserve text input for AltGr and text-producing Option chords.
        _ if m.control && m.alt && key.key_char.is_some() => None,
        _ if cfg!(target_os = "macos")
            && m.alt
            && key.key_char.as_ref().is_some_and(|text| !text.is_ascii()) =>
        {
            None
        }
        name if (m.control || m.alt) && name.len() == 1 => {
            Some(u32::from(name.as_bytes()[0].to_ascii_lowercase()))
        }
        _ => None,
    }
}

fn control_byte(key: &str, key_char: Option<&str>) -> Option<u8> {
    if let Some(b'a'..=b'z') = key.as_bytes().first().copied().filter(|_| key.len() == 1) {
        return Some(key.as_bytes()[0] - b'a' + 1);
    }
    match key_char.unwrap_or(key) {
        " " | "space" | "@" | "2" => Some(0),
        "[" | "3" => Some(27),
        "\\" | "4" => Some(28),
        "]" | "5" => Some(29),
        "^" | "6" => Some(30),
        "_" | "-" | "7" => Some(31),
        "?" | "8" => Some(127),
        _ => None,
    }
}

pub(super) fn mouse_button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

pub(super) fn mouse_bytes(
    mode: TermMode,
    button: u8,
    column: usize,
    row: usize,
    release: bool,
    m: Modifiers,
) -> Option<Vec<u8>> {
    let modifiers = 4 * u8::from(m.shift) + 8 * u8::from(m.alt) + 16 * u8::from(m.control);
    let column = column + 1;
    let row = row + 1;
    if mode.contains(TermMode::SGR_MOUSE) {
        let action = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{column};{row}{action}", button + modifiers).into_bytes());
    }

    let button = if release { 3 } else { button } + modifiers + 32;
    let mut bytes = vec![0x1b, b'[', b'M', button];
    if mode.contains(TermMode::UTF8_MOUSE) {
        for coordinate in [column, row] {
            if coordinate > 2015 {
                return None;
            }
            let mut buffer = [0; 4];
            bytes.extend_from_slice(
                char::from_u32(coordinate as u32 + 32)?
                    .encode_utf8(&mut buffer)
                    .as_bytes(),
            );
        }
    } else {
        bytes.push(u8::try_from(column + 32).ok()?);
        bytes.push(u8::try_from(row + 32).ok()?);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_key_bytes(key: &Keystroke, mode: TermMode) -> Vec<u8> {
        key_bytes(key, mode, ShiftEnterPreference::default()).unwrap()
    }

    #[test]
    fn encodes_application_cursor_and_modifiers() {
        let key = Keystroke::parse("up").unwrap();
        assert_eq!(default_key_bytes(&key, TermMode::APP_CURSOR), b"\x1bOA");
        let key = Keystroke::parse("ctrl-left").unwrap();
        assert_eq!(default_key_bytes(&key, TermMode::APP_CURSOR), b"\x1b[1;5D");
        let key = Keystroke::parse("ctrl-c").unwrap();
        assert_eq!(default_key_bytes(&key, TermMode::empty()), b"\x03");
    }

    #[test]
    fn disambiguates_modified_enter_and_escape_only_when_negotiated() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES;
        let key = Keystroke::parse("shift-enter").unwrap();
        assert_eq!(default_key_bytes(&key, mode), b"\x1b[13;2u");
        assert_eq!(default_key_bytes(&key, TermMode::empty()), b"\x1b\r");
        for (preference, expected) in [
            (ShiftEnterPreference::Enter, b"\r"),
            (ShiftEnterPreference::LineFeed, b"\n"),
        ] {
            assert_eq!(
                key_bytes(&key, TermMode::empty(), preference).unwrap(),
                expected
            );
            assert_eq!(key_bytes(&key, mode, preference).unwrap(), b"\x1b[13;2u");
        }
        let key = Keystroke::parse("enter").unwrap();
        assert_eq!(default_key_bytes(&key, mode), b"\r");
        let key = Keystroke::parse("escape").unwrap();
        assert_eq!(default_key_bytes(&key, mode), b"\x1b[27u");
        let key = Keystroke::parse("ctrl-c").unwrap();
        assert_eq!(default_key_bytes(&key, mode), b"\x1b[99;5u");
        let key = Keystroke::parse("f3").unwrap();
        assert_eq!(default_key_bytes(&key, mode), b"\x1b[13~");
    }

    #[test]
    fn brackets_paste_and_strips_escape_bytes() {
        assert_eq!(paste("a\n\x1bb", true), b"\x1b[200~a\nb\x1b[201~",);
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc");
    }

    #[test]
    fn confirms_only_direct_unbracketed_pastes_with_line_breaks() {
        for text in ["one\ntwo", "one\r\ntwo", "one\rtwo", "one\n"] {
            assert!(requires_paste_confirmation(text, false, true));
            assert!(!requires_paste_confirmation(text, true, true));
            assert!(!requires_paste_confirmation(text, false, false));
        }
        assert!(!requires_paste_confirmation("one line", false, true));
    }

    #[test]
    fn uses_one_based_mouse_coordinates() {
        assert_eq!(
            mouse_bytes(TermMode::SGR_MOUSE, 0, 2, 4, true, Modifiers::default()).unwrap(),
            b"\x1b[<0;3;5m",
        );
    }
}
