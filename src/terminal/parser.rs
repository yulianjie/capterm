//! Delegate VT actions to Alacritty while negotiating only the keyboard
//! enhancement the frontend encodes.

use std::{collections::VecDeque, time::Instant};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::Dimensions,
    index::Line,
    term::TermMode,
    vte::ansi::{
        Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
        KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb,
        StandardCharset, TabulationClearMode, cursor_icon::CursorIcon,
    },
};
use chrono::{DateTime, Local};

use super::{
    TerminalEvents, TimestampFormat, format_duration,
    timeline::{self, Stamp, Timeline},
};

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
    pub(super) tracking: Option<Tracking<'a>>,
}

pub(super) struct Tracking<'a> {
    pub(super) timeline: &'a mut Timeline,
    pub(super) reflow: &'a mut Option<Vec<Option<Stamp>>>,
    pub(super) primary_log: &'a mut Option<Box<str>>,
    pub(super) timestamp_format: TimestampFormat,
    pub(super) reflowed: &'a mut bool,
    pub(super) redraw: &'a mut Option<usize>,
    pub(super) fullscreen: &'a mut Option<(Instant, Option<String>)>,
    pub(super) program: Option<&'a str>,
    pub(super) inline_agent: bool,
    pub(super) now: DateTime<Local>,
    pub(super) last_input: Option<(Line, bool)>,
}

impl ParserHandler<'_> {
    fn keyboard_screen(&mut self) -> &mut KeyboardScreen {
        &mut self.keyboard.0[screen_index(*self.term.mode())]
    }

    fn reset_input_run(&mut self) {
        if let Some(track) = &mut self.tracking {
            track.last_input = None;
        }
    }

    fn touch_edited_row(&mut self) {
        self.reset_input_run();
        if !self.term.mode().contains(TermMode::ALT_SCREEN)
            && let Some(track) = &mut self.tracking
        {
            let grid = self.term.grid();
            let line = grid.cursor.point.line;
            if let Some(stamp) = track.timeline.stamp(grid, line) {
                track
                    .timeline
                    .touch(grid, line, track.now, false, stamp.live);
            }
        }
    }
}

macro_rules! forward_actions {
    ($(fn $method:ident($($arg:ident: $ty:ty),*);)*) => {
        $(fn $method(&mut self, $($arg: $ty),*) {
            self.reset_input_run();
            Handler::$method(self.term, $($arg),*);
        })*
    };
}

impl Handler for ParserHandler<'_> {
    forward_actions! {
        fn set_cursor_style(style: Option<CursorStyle>);
        fn set_cursor_shape(shape: CursorShape);
        fn goto(line: i32, column: usize);
        fn goto_line(line: i32);
        fn goto_col(column: usize);
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
        fn bell();
        fn substitute();
        fn set_horizontal_tabstop();
        fn scroll_up(count: usize);
        fn scroll_down(count: usize);
        fn insert_blank_lines(count: usize);
        fn delete_lines(count: usize);
        fn move_backward_tabs(count: u16);
        fn move_forward_tabs(count: u16);
        fn save_cursor_position();
        fn restore_cursor_position();
        fn clear_tabs(mode: TabulationClearMode);
        fn set_tabs(interval: u16);
        fn reverse_index();
        fn terminal_attribute(attribute: Attr);
        fn set_mode(mode: Mode);
        fn unset_mode(mode: Mode);
        fn report_mode(mode: Mode);
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

    fn insert_blank(&mut self, count: usize) {
        self.term.insert_blank(count);
        self.touch_edited_row();
    }

    fn erase_chars(&mut self, count: usize) {
        self.term.erase_chars(count);
        self.touch_edited_row();
    }

    fn delete_chars(&mut self, count: usize) {
        self.term.delete_chars(count);
        self.touch_edited_row();
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        self.term.clear_line(mode);
        self.touch_edited_row();
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        self.reset_input_run();
        let clears_view = matches!(mode, ClearMode::All);
        self.term.clear_screen(mode);
        if clears_view
            && !self.term.mode().contains(TermMode::ALT_SCREEN)
            && let Some(track) = &mut self.tracking
        {
            let grid = self.term.grid();
            for line in (0..grid.screen_lines() as i32).map(Line) {
                track.timeline.clear_reused(grid, line);
            }
            *track.redraw = None;
        }
    }

    fn set_title(&mut self, title: Option<String>) {
        if let Some(track) = &mut self.tracking
            && let Some((_, label)) = track.fullscreen
            && let Some(title) = title.as_ref().filter(|title| !title.trim().is_empty())
        {
            *label = Some(
                title
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(120)
                    .collect(),
            );
        }
        self.term.set_title(title);
    }

    fn input(&mut self, character: char) {
        let Some(track) = self
            .tracking
            .as_mut()
            .filter(|_| !self.term.mode().contains(TermMode::ALT_SCREEN))
        else {
            self.term.input(character);
            return;
        };
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let before = cursor.line;
        let needs_wrap = grid.cursor.input_needs_wrap;
        let replaces_agent_cell = track.inline_agent && !needs_wrap && grid[cursor].c != ' ';
        // An ASCII character inside the same physical row cannot change its
        // logical line number. The row has already been stamped for this batch.
        if character.is_ascii()
            && !needs_wrap
            && let Some((line, live)) = track.last_input
            && line == before
            && live == replaces_agent_cell
        {
            self.term.input(character);
            return;
        }
        let key = timeline::row_key(grid, before);
        self.term.input(character);
        let grid = self.term.grid();
        let after = grid.cursor.point.line;
        let after_key = timeline::row_key(grid, after);
        let fresh = before == after && key != after_key;
        let live = !fresh
            && (*track.redraw == Some(after_key) || (before == after && replaces_agent_cell));
        track.timeline.touch(grid, after, track.now, fresh, live);
        // A pending redraw can belong to a different row after wrapping.
        track.last_input = track.redraw.is_none().then_some((after, live));
    }

    fn carriage_return(&mut self) {
        self.reset_input_run();
        if let Some(track) = &mut self.tracking {
            let grid = self.term.grid();
            let line = grid.cursor.point.line;
            *track.redraw = (grid.cursor.point.column.0 > 0).then(|| timeline::row_key(grid, line));
        }
        self.term.carriage_return();
    }

    fn linefeed(&mut self) {
        self.reset_input_run();
        let primary = !self.term.mode().contains(TermMode::ALT_SCREEN);
        let grid = self.term.grid();
        let line = grid.cursor.point.line;
        let key = timeline::row_key(grid, line);
        if let Some(track) = &mut self.tracking {
            *track.redraw = None;
            if primary {
                track.timeline.ensure(grid, line, track.now);
            }
        }
        self.term.linefeed();
        if primary && let Some(track) = &mut self.tracking {
            let grid = self.term.grid();
            if grid.cursor.point.line == line && timeline::row_key(grid, line) != key {
                track.timeline.clear_reused(grid, line);
            }
        }
    }

    fn newline(&mut self) {
        self.linefeed();
        if self.term.mode().contains(TermMode::LINE_FEED_NEW_LINE) {
            self.carriage_return();
        }
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        self.reset_input_run();
        if mode.raw() == 1049
            && !self.term.mode().contains(TermMode::ALT_SCREEN)
            && let Some(track) = &mut self.tracking
        {
            track.timeline.output(self.term.grid(), track.now);
            *track.reflow = Some(track.timeline.before_resize(self.term.grid()));
            *track.primary_log = Some(
                track
                    .timeline
                    .formatted_log(self.term.grid(), track.timestamp_format)
                    .into_boxed_str(),
            );
            *track.redraw = None;
            *track.fullscreen = Some((Instant::now(), track.program.map(str::to_owned)));
        }
        self.term.set_private_mode(mode);
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        self.reset_input_run();
        let was_alternate = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.term.unset_private_mode(mode);
        if mode.raw() == 1049
            && was_alternate
            && let Some(track) = &mut self.tracking
        {
            if let Some(logical) = track.reflow.take()
                && std::mem::take(track.reflowed)
            {
                track.timeline.resized(self.term.grid(), logical);
            }
            *track.primary_log = None;
            *track.redraw = None;
            if let Some((started, name)) = track.fullscreen.take() {
                let label =
                    name.map_or_else(|| "全屏程序".into(), |name| format!("全屏程序 {name}"));
                track.timeline.system_at_cursor(
                    self.term.grid(),
                    track.now,
                    format!("已退出{label}，用时 {}", format_duration(started.elapsed())),
                );
            }
        }
    }

    fn reset_state(&mut self) {
        self.reset_input_run();
        *self.keyboard = KeyboardProtocol::default();
        self.term.reset_state();
        if let Some(track) = &mut self.tracking {
            track.timeline.reset();
            *track.primary_log = None;
            *track.fullscreen = None;
            *track.reflow = None;
            *track.reflowed = false;
            *track.redraw = None;
        }
        self.events.send_event(Event::ResetTitle);
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
    use chrono::TimeZone;

    struct TerminalFixture {
        events: TerminalEvents,
        term: Term<TerminalEvents>,
        parser: Processor,
        keyboard: KeyboardProtocol,
        timeline: Timeline,
        reflow: Option<Vec<Option<Stamp>>>,
        primary_log: Option<Box<str>>,
        reflowed: bool,
        redraw: Option<usize>,
        fullscreen: Option<(Instant, Option<String>)>,
    }

    impl TerminalFixture {
        fn new(cols: u16, rows: u16) -> Self {
            let events = TerminalEvents::default();
            let term = Term::new(
                Config {
                    scrolling_history: 2,
                    ..Config::default()
                },
                &TerminalSize {
                    cols,
                    rows,
                    ..TerminalSize::default()
                },
                events.clone(),
            );
            Self {
                events,
                term,
                parser: Processor::new(),
                keyboard: KeyboardProtocol::default(),
                timeline: Timeline::default(),
                reflow: None,
                primary_log: None,
                reflowed: false,
                redraw: None,
                fullscreen: None,
            }
        }

        fn feed(&mut self, bytes: &[u8], second: u32) {
            let now = Local.with_ymd_and_hms(2026, 11, 7, 10, 0, second).unwrap();
            self.parser.advance(
                &mut ParserHandler {
                    term: &mut self.term,
                    keyboard: &mut self.keyboard,
                    events: &self.events,
                    tracking: Some(Tracking {
                        timeline: &mut self.timeline,
                        reflow: &mut self.reflow,
                        primary_log: &mut self.primary_log,
                        timestamp_format: TimestampFormat::Seconds,
                        reflowed: &mut self.reflowed,
                        redraw: &mut self.redraw,
                        fullscreen: &mut self.fullscreen,
                        program: None,
                        inline_agent: false,
                        now,
                        last_input: None,
                    }),
                },
                bytes,
            );
            if !self.term.mode().contains(TermMode::ALT_SCREEN) {
                self.timeline.output(self.term.grid(), now);
            }
        }

        fn stamp(&self, line: i32) -> (u64, String, bool) {
            let stamp = self.timeline.stamp(self.term.grid(), Line(line)).unwrap();
            (
                stamp.number,
                stamp.time.format("%H:%M:%S").to_string(),
                stamp.live,
            )
        }
    }

    #[test]
    fn recycled_rows_keep_monotonic_numbers_and_fresh_times_for_identical_text() {
        let mut terminal = TerminalFixture::new(8, 2);
        terminal.feed(b"same", 1);
        for number in 2..=12 {
            terminal.feed(b"\r\nsame", number);
        }
        for (line, number) in [(-2, 9), (-1, 10), (0, 11), (1, 12)] {
            assert_eq!(
                terminal.stamp(line),
                (number, format!("10:00:{number:02}"), false)
            );
        }
    }

    #[test]
    fn live_redraw_updates_time_without_renumbering_and_history_freezes_it() {
        let mut terminal = TerminalFixture::new(12, 2);
        terminal.feed(b"progress", 1);
        terminal.feed(b"\r", 2);
        assert_eq!(terminal.stamp(0), (1, "10:00:01".into(), false));
        terminal.feed(b"finished", 3);
        assert_eq!(terminal.stamp(0), (1, "10:00:03".into(), true));
        terminal.feed(b"\r\nnew\r\nlast", 4);
        assert_eq!(terminal.stamp(-1), (1, "10:00:03".into(), false));
    }

    #[test]
    fn soft_wrap_and_reflow_share_one_logical_number_and_last_write_time() {
        let mut terminal = TerminalFixture::new(4, 4);
        terminal.feed(b"abcd", 1);
        terminal.feed(b"ef\r\nghi", 2);
        assert_eq!(terminal.stamp(0), (1, "10:00:02".into(), false));
        assert_eq!(terminal.stamp(1), (1, "10:00:02".into(), false));
        assert_eq!(terminal.stamp(2), (2, "10:00:02".into(), false));
        let before = terminal.timeline.before_resize(terminal.term.grid());
        terminal.term.resize(TerminalSize {
            cols: 8,
            rows: 4,
            ..TerminalSize::default()
        });
        terminal.timeline.resized(terminal.term.grid(), before);
        assert_eq!(timeline::row_text(terminal.term.grid(), Line(0)), "abcdef");
        assert_eq!(terminal.stamp(0), (1, "10:00:02".into(), false));
        assert_eq!(terminal.stamp(1), (2, "10:00:02".into(), false));
    }

    #[test]
    fn explicit_scroll_keeps_input_numbering_in_order_within_one_batch() {
        let mut terminal = TerminalFixture::new(20, 3);
        terminal.feed(b"old\x1b[Snew\x1b[2Bend", 1);
        assert_eq!(terminal.stamp(-1).0, 1);
        assert_eq!(terminal.stamp(0).0, 2);
        assert_eq!(terminal.stamp(2).0, 3);
    }

    #[test]
    fn alternate_screen_has_no_effect_on_primary_numbers_even_in_one_batch() {
        let mut terminal = TerminalFixture::new(40, 3);
        terminal.feed(b"first\r\n\x1b[?1049h\x1b[Halt\r\nmore\x1b[?1049lsecond", 1);
        assert_eq!(terminal.stamp(0).0, 1);
        assert_eq!(terminal.stamp(1).0, 2);
        assert!(
            terminal.timeline.systems.get(&2).unwrap()[0]
                .text
                .starts_with("已退出全屏程序")
        );
    }

    #[test]
    fn active_alternate_screen_keeps_the_primary_formatted_log() {
        let mut terminal = TerminalFixture::new(40, 3);
        terminal.feed(b"primary\r\n\x1b[?1049h\x1b[Halternate", 1);
        let log = terminal.primary_log.as_deref().unwrap();
        assert!(log.contains("1 10:00:01  primary"));
        assert!(!log.contains("alternate"));
        terminal.feed(b"\x1b[?1049lresumed", 2);
        assert!(terminal.primary_log.is_none());
        assert_eq!(terminal.stamp(1).0, 2);
    }

    #[test]
    fn recognizes_powershell_prompt_before_and_after_a_fullscreen_program() {
        let mut terminal = TerminalFixture::new(80, 6);
        terminal.feed(b"PS C:\\Users\\fixture> ", 1);
        assert_eq!(terminal.timeline.latest_prompt(), Some(1));
        terminal.timeline.start_fallback(
            terminal
                .timeline
                .stamp(terminal.term.grid(), Line(0))
                .unwrap()
                .time,
        );
        terminal.feed(b"opencode\r\n\x1b[?1049h\x1b[HOpenCode", 2);
        assert!(terminal.term.mode().contains(TermMode::ALT_SCREEN));
        terminal.feed(b"\x1b[?1049l\x1b[?25hPS C:\\Users\\fixture> ", 3);
        assert!(!terminal.term.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(terminal.timeline.latest_prompt(), Some(2));
        assert_eq!(
            terminal.timeline.commands.get(&1).unwrap().last_line,
            Some(1)
        );
    }

    #[test]
    fn captures_the_full_powershell_path_when_the_prompt_wraps() {
        let mut terminal = TerminalFixture::new(16, 6);
        terminal.feed(b"PS C:\\Users\\fixture\\Project Files> ", 1);
        let number = terminal.timeline.latest_prompt().unwrap();
        assert_eq!(
            terminal.timeline.powershell_prompt(number),
            Some(r"PS C:\Users\fixture\Project Files>")
        );
    }

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
            tracking: None,
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
            tracking: None,
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
            tracking: None,
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
