//! Delegate VT actions to Alacritty while negotiating only the keyboard
//! enhancement the frontend encodes.

use std::collections::VecDeque;

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    term::TermMode,
    vte::ansi::{
        Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
        KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb,
        StandardCharset, TabulationClearMode, cursor_icon::CursorIcon,
    },
};

use super::TerminalEvents;

const KEYBOARD_STACK_LIMIT: usize = 128;
const SUPPORTED: KeyboardModes = KeyboardModes::DISAMBIGUATE_ESC_CODES;

#[derive(Default)]
struct KeyboardScreen {
    current: KeyboardModes,
    saved: VecDeque<KeyboardModes>,
}

#[derive(Default)]
pub(super) struct KeyboardProtocol([KeyboardScreen; 2]);

impl KeyboardProtocol {
    pub(super) fn mode(&self, term_mode: TermMode) -> TermMode {
        self.0[screen_index(term_mode)].current.into()
    }
}

fn screen_index(mode: TermMode) -> usize {
    usize::from(mode.contains(TermMode::ALT_SCREEN))
}

pub(super) struct ParserHandler<'a> {
    pub(super) term: &'a mut Term<TerminalEvents>,
    pub(super) keyboard: &'a mut KeyboardProtocol,
    pub(super) events: &'a TerminalEvents,
}

impl ParserHandler<'_> {
    fn keyboard_screen(&mut self) -> &mut KeyboardScreen {
        &mut self.keyboard.0[screen_index(*self.term.mode())]
    }
}

macro_rules! forward_actions {
    ($(fn $method:ident($($arg:ident: $ty:ty),*);)*) => {
        $(fn $method(&mut self, $($arg: $ty),*) {
            Handler::$method(self.term, $($arg),*);
        })*
    };
}

impl Handler for ParserHandler<'_> {
    forward_actions! {
        fn set_title(title: Option<String>);
        fn set_cursor_style(style: Option<CursorStyle>);
        fn set_cursor_shape(shape: CursorShape);
        fn input(character: char);
        fn goto(line: i32, column: usize);
        fn goto_line(line: i32);
        fn goto_col(column: usize);
        fn insert_blank(count: usize);
        fn move_up(count: usize);
        fn move_down(count: usize);
        fn identify_terminal(intermediate: Option<char>);
        fn device_status(status: usize);
        fn move_forward(count: usize);
        fn move_backward(count: usize);
        fn move_down_and_cr(count: usize);
        fn move_up_and_cr(count: usize);
        fn put_tab(count: u16);
        fn backspace();
        fn carriage_return();
        fn linefeed();
        fn bell();
        fn substitute();
        fn newline();
        fn set_horizontal_tabstop();
        fn scroll_up(count: usize);
        fn scroll_down(count: usize);
        fn insert_blank_lines(count: usize);
        fn delete_lines(count: usize);
        fn erase_chars(count: usize);
        fn delete_chars(count: usize);
        fn move_backward_tabs(count: u16);
        fn move_forward_tabs(count: u16);
        fn save_cursor_position();
        fn restore_cursor_position();
        fn clear_line(mode: LineClearMode);
        fn clear_screen(mode: ClearMode);
        fn clear_tabs(mode: TabulationClearMode);
        fn set_tabs(interval: u16);
        fn reverse_index();
        fn terminal_attribute(attribute: Attr);
        fn set_mode(mode: Mode);
        fn unset_mode(mode: Mode);
        fn report_mode(mode: Mode);
        fn set_private_mode(mode: PrivateMode);
        fn unset_private_mode(mode: PrivateMode);
        fn report_private_mode(mode: PrivateMode);
        fn set_scrolling_region(top: usize, bottom: Option<usize>);
        fn set_keypad_application_mode();
        fn unset_keypad_application_mode();
        fn set_active_charset(charset: CharsetIndex);
        fn configure_charset(index: CharsetIndex, charset: StandardCharset);
        fn set_color(index: usize, color: Rgb);
        fn dynamic_color_sequence(prefix: String, index: usize, suffix: &str);
        fn reset_color(index: usize);
        fn clipboard_store(selection: u8, data: &[u8]);
        fn clipboard_load(selection: u8, suffix: &str);
        fn decaln();
        fn push_title();
        fn pop_title();
        fn text_area_size_pixels();
        fn text_area_size_chars();
        fn set_hyperlink(link: Option<Hyperlink>);
        fn set_mouse_cursor_icon(icon: CursorIcon);
        fn set_modify_other_keys(mode: ModifyOtherKeys);
        fn report_modify_other_keys();
    }

    fn reset_state(&mut self) {
        *self.keyboard = KeyboardProtocol::default();
        self.term.reset_state();
    }

    fn report_keyboard_mode(&mut self) {
        let flags = self.keyboard_screen().current.bits();
        self.events
            .send_event(Event::PtyWrite(format!("\x1b[?{flags}u")));
    }

    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        let screen = self.keyboard_screen();
        if screen.saved.len() == KEYBOARD_STACK_LIMIT {
            screen.saved.pop_front();
        }
        screen.saved.push_back(screen.current);
        screen.current = mode & SUPPORTED;
    }

    fn pop_keyboard_modes(&mut self, count: u16) {
        let screen = self.keyboard_screen();
        let count = usize::from(count);
        if count == 0 {
            return;
        }
        let keep = screen.saved.len().checked_sub(count);
        screen.current = keep
            .and_then(|keep| screen.saved.get(keep).copied())
            .unwrap_or_default();
        screen.saved.truncate(keep.unwrap_or(0));
    }

    fn set_keyboard_mode(&mut self, mode: KeyboardModes, apply: KeyboardModesApplyBehavior) {
        let screen = self.keyboard_screen();
        let mode = mode & SUPPORTED;
        screen.current = match apply {
            KeyboardModesApplyBehavior::Replace => mode,
            KeyboardModesApplyBehavior::Union => screen.current | mode,
            KeyboardModesApplyBehavior::Difference => screen.current - mode,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalSize;
    use alacritty_terminal::{
        index::{Column, Line},
        term::{Config, cell::Flags},
        vte::ansi::{Color, NamedColor, Processor},
    };

    #[test]
    fn negotiates_only_implemented_keyboard_flags_and_restores_primary() {
        let events = TerminalEvents::default();
        let mut term = Term::new(Config::default(), &TerminalSize::default(), events.clone());
        let mut keyboard = KeyboardProtocol::default();
        let mut parser: Processor = Processor::new();
        let mut handler = ParserHandler {
            term: &mut term,
            keyboard: &mut keyboard,
            events: &events,
        };
        parser.advance(&mut handler, b"\x1b[>3");
        parser.advance(&mut handler, b"1u\x1b[?u");
        assert_eq!(
            handler.keyboard.mode(*handler.term.mode()),
            TermMode::DISAMBIGUATE_ESC_CODES
        );
        parser.advance(&mut handler, b"\x1b[?1049h\x1b[?u");
        assert!(handler.keyboard.mode(*handler.term.mode()).is_empty());
        parser.advance(&mut handler, b"\x1b[?1049l\x1b[?u\x1b[<u\x1b[?u");
        let replies: Vec<_> = events
            .0
            .borrow()
            .iter()
            .filter_map(|event| match event {
                Event::PtyWrite(reply) => Some(reply.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(replies, ["\x1b[?1u", "\x1b[?0u", "\x1b[?1u", "\x1b[?0u"]);
    }

    #[test]
    fn remote_keyboard_stack_is_bounded() {
        let events = TerminalEvents::default();
        let mut term = Term::new(Config::default(), &TerminalSize::default(), events.clone());
        let mut keyboard = KeyboardProtocol::default();
        let mut parser: Processor = Processor::new();
        let mut handler = ParserHandler {
            term: &mut term,
            keyboard: &mut keyboard,
            events: &events,
        };
        parser.advance(&mut handler, &b"\x1b[>1u".repeat(4097));
        assert_eq!(handler.keyboard_screen().saved.len(), KEYBOARD_STACK_LIMIT);
        parser.advance(&mut handler, b"\x1b[<4097u");
        assert!(handler.keyboard.mode(*handler.term.mode()).is_empty());
    }

    #[test]
    fn forwards_fragmented_utf8_styling_and_alternate_screen_actions() {
        let events = TerminalEvents::default();
        let mut term = Term::new(Config::default(), &TerminalSize::default(), events.clone());
        let mut keyboard = KeyboardProtocol::default();
        let mut parser: Processor = Processor::new();
        let mut handler = ParserHandler {
            term: &mut term,
            keyboard: &mut keyboard,
            events: &events,
        };
        parser.advance(&mut handler, b"\x1b[3");
        parser.advance(&mut handler, b"1m\xe7");
        parser.advance(&mut handler, b"\x95\x8c");
        let first = &handler.term.grid()[Line(0)][Column(0)];
        assert_eq!(first.c, '界');
        assert_eq!(first.fg, Color::Named(NamedColor::Red));
        assert!(first.flags.contains(Flags::WIDE_CHAR));
        parser.advance(&mut handler, b"\x1b[?1049h\x1b[Halt");
        assert_eq!(handler.term.grid()[Line(0)][Column(0)].c, 'a');
        parser.advance(&mut handler, b"\x1b[?1049l");
        assert_eq!(handler.term.grid()[Line(0)][Column(0)].c, '界');
    }
}
