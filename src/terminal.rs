//! A native VT viewport driven by raw bytes from an interactive SSH PTY.

mod cell_graphics;
pub(crate) mod highlight;
mod input;
mod osc;
mod paint;
mod parser;
mod prompt;
mod search;
mod timeline;

use std::{
    cell::RefCell,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as CellPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, Osc52, TermMode},
    vte::ansi::Processor,
};
use chrono::{DateTime, Local};
use gpui_kit::{
    App, AppContext, Bounds, ClipboardEntry, ClipboardItem, ContentMask, Context, Div,
    ElementInputHandler, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollEvent, StatefulInteractiveElement, Styled, Subscription, Task, UTF16Selection, Window,
    canvas,
    component::{
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt, PopupMenuItem},
    },
    div, point, px, rgb, size,
};

use highlight::LinuxHighlights;
use osc::{Marker, Scanner};
use paint::{Frame, FrameOptions, Palette, color_components, terminal_font};
use parser::{KeyboardProtocol, ParserHandler, Tracking};
use search::Search;
use timeline::{Stamp, Timeline};

pub use timeline::TimestampFormat;

gpui_kit::actions!(terminal, [TerminalTab, TerminalBackTab]);

const KEY_CONTEXT: &str = "CapTermTerminal";

pub(crate) fn init(cx: &mut App) {
    // GPUI dispatches the root's focus traversal bindings before raw key-down listeners.
    cx.bind_keys([
        KeyBinding::new("tab", TerminalTab, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", TerminalBackTab, Some(KEY_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

#[derive(Debug)]
pub enum TerminalCommand {
    Data(Vec<u8>),
    Resize(TerminalSize),
}

pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

impl EventEmitter<ClipboardImage> for TerminalView {}

#[derive(Clone, Default)]
struct TerminalEvents(Rc<RefCell<Vec<Event>>>);

impl EventListener for TerminalEvents {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    bounds: Bounds<Pixels>,
    grid: Bounds<Pixels>,
    number_width: Pixels,
    time_width: Pixels,
    marker_width: Pixels,
    cell_width: Pixels,
    font_size: Pixels,
    line_height: Pixels,
    size: TerminalSize,
}

impl Geometry {
    fn new(
        bounds: Bounds<Pixels>,
        cell_width: Pixels,
        show_numbers: bool,
        show_time: bool,
        timestamp_format: TimestampFormat,
        compact: bool,
        alternate: bool,
        font_size: Pixels,
        line_height: Pixels,
    ) -> Self {
        let number_width = if show_numbers {
            px(if compact { 42. } else { 46. })
        } else {
            px(0.)
        };
        let time_width = if show_time {
            px(match timestamp_format {
                TimestampFormat::Seconds => 64.,
                TimestampFormat::Milliseconds => 94.,
                TimestampFormat::Relative => 72.,
            })
        } else {
            px(0.)
        };
        let marker_width = if show_numbers || show_time {
            px(16.)
        } else {
            px(10.)
        };
        let top_padding = px(4.);
        let ruler_width = if alternate { px(0.) } else { px(12.) };
        let width =
            (bounds.size.width - number_width - time_width - marker_width - ruler_width - px(4.))
                .max(px(0.));
        let height = (bounds.size.height - top_padding).max(px(0.));
        let cols = (width / cell_width).floor().clamp(2., 1000.) as u16;
        let rows = (height / line_height).floor().clamp(1., 1000.) as u16;
        let size_px = size(cell_width * f32::from(cols), line_height * f32::from(rows));
        let grid = Bounds::new(
            point(
                bounds.origin.x + number_width + time_width + marker_width,
                bounds.origin.y + top_padding,
            ),
            size_px,
        );
        let size = TerminalSize {
            cols,
            rows,
            pixel_width: f32::from(size_px.width).round() as u16,
            pixel_height: f32::from(size_px.height).round() as u16,
        };
        Self {
            bounds,
            grid,
            number_width,
            time_width,
            marker_width,
            cell_width,
            font_size,
            line_height,
            size,
        }
    }

    fn gutter_width(self) -> Pixels {
        self.number_width + self.time_width
    }

    fn ruler_contains(self, position: Point<Pixels>) -> bool {
        self.bounds.contains(&position)
            && position.x >= self.bounds.origin.x + self.bounds.size.width - px(12.)
    }

    fn locate(self, position: Point<Pixels>, clamp: bool) -> Option<(usize, usize, Side)> {
        if !clamp && !self.grid.contains(&position) {
            return None;
        }
        let x = (position.x - self.grid.origin.x) / self.cell_width;
        let y = (position.y - self.grid.origin.y) / self.line_height;
        let row = (y.floor() as usize).min(self.size.screen_lines() - 1);
        let column = (x.floor() as usize).min(self.size.columns() - 1);
        let side = if x - (column as f32) < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        Some((column, row, side))
    }
}

pub struct TerminalView {
    term: Term<TerminalEvents>,
    parser: Processor,
    keyboard: KeyboardProtocol,
    sync_flush: Option<(Instant, Task<()>)>,
    events: TerminalEvents,
    send: Box<dyn Fn(TerminalCommand)>,
    size: TerminalSize,
    geometry: Option<Geometry>,
    line_map: Vec<Option<Line>>,
    timeline_offset: usize,
    timeline_overflow: usize,
    timeline: Timeline,
    linux_highlights: Option<Rc<LinuxHighlights>>,
    primary_reflowed: bool,
    primary_snapshot: Option<Vec<Option<Stamp>>>,
    primary_log: Option<String>,
    redraw: Option<usize>,
    resize_pending: Option<Task<()>>,
    scanner: Scanner,
    sync_markers: Vec<Marker>,
    search: Search,
    title: Option<String>,
    title_agent: Option<&'static str>,
    launched_agent: Option<&'static str>,
    agent_started: Option<Instant>,
    cwd: Option<String>,
    notification: Option<String>,
    notification_serial: u64,
    prompt_serial: u64,
    prompt: Option<(u64, String)>,
    fullscreen: Option<(Instant, Option<String>)>,
    last_program: Option<String>,
    clipboard_request: Option<String>,
    font_family: Option<String>,
    font_metrics: Option<(Font, Pixels)>,
    focus: FocusHandle,
    focus_subscriptions: Option<[Subscription; 2]>,
    preedit: String,
    selecting: bool,
    reported_button: Option<MouseButton>,
    last_mouse_cell: Option<(usize, usize)>,
    wheel_remainder: f32,
    manual_raw: bool,
    show_numbers: bool,
    show_time: bool,
    timestamp_format: TimestampFormat,
    idle_threshold: Duration,
    dim_repeated: bool,
    font_size: Pixels,
    line_height: Pixels,
    compact: bool,
    light: bool,
}

impl TerminalView {
    pub fn new(send: impl Fn(TerminalCommand) + 'static, cx: &mut Context<Self>) -> Self {
        let config = Config {
            osc52: Osc52::OnlyCopy,
            scrolling_history: 100_000,
            ..Config::default()
        };
        let size = TerminalSize::default();
        let events = TerminalEvents::default();
        Self {
            term: Term::new(config, &size, events.clone()),
            parser: Processor::new(),
            keyboard: KeyboardProtocol::default(),
            sync_flush: None,
            events,
            send: Box::new(send),
            size,
            geometry: None,
            line_map: Vec::new(),
            timeline_offset: 0,
            timeline_overflow: 0,
            timeline: Timeline::default(),
            linux_highlights: None,
            primary_reflowed: false,
            primary_snapshot: None,
            primary_log: None,
            redraw: None,
            resize_pending: None,
            scanner: Scanner::default(),
            sync_markers: Vec::new(),
            search: Search::default(),
            title: None,
            title_agent: None,
            launched_agent: None,
            agent_started: None,
            cwd: None,
            notification: None,
            notification_serial: 0,
            prompt_serial: 0,
            prompt: None,
            fullscreen: None,
            last_program: None,
            clipboard_request: None,
            font_family: None,
            font_metrics: None,
            focus: cx.focus_handle(),
            focus_subscriptions: None,
            preedit: String::new(),
            selecting: false,
            reported_button: None,
            last_mouse_cell: None,
            wheel_remainder: 0.,
            manual_raw: false,
            show_numbers: true,
            show_time: true,
            timestamp_format: TimestampFormat::default(),
            idle_threshold: Duration::from_secs(60),
            dim_repeated: true,
            font_size: px(13.),
            line_height: px(20.),
            compact: false,
            light: false,
        }
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE)
    }

    pub(crate) fn set_linux_highlights(
        &mut self,
        rules: Option<Rc<LinuxHighlights>>,
        cx: &mut Context<Self>,
    ) {
        self.linux_highlights = rules;
        cx.notify();
    }

    pub fn raw(&self) -> bool {
        self.manual_raw || self.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn set_raw(&mut self, raw: bool, cx: &mut Context<Self>) {
        if self.manual_raw != raw {
            self.manual_raw = raw;
            if raw {
                self.term.scroll_display(Scroll::Bottom);
                self.timeline_offset = 0;
            }
            cx.notify();
        }
    }

    pub fn set_gutters(&mut self, numbers: bool, timestamps: bool, cx: &mut Context<Self>) {
        if (self.show_numbers, self.show_time) != (numbers, timestamps) {
            self.show_numbers = numbers;
            self.show_time = timestamps;
            cx.notify();
        }
    }

    pub fn set_timeline_options(
        &mut self,
        format: TimestampFormat,
        idle_threshold: Duration,
        dim_repeated: bool,
        cx: &mut Context<Self>,
    ) {
        self.timestamp_format = format;
        self.idle_threshold = idle_threshold;
        self.dim_repeated = dim_repeated;
        cx.notify();
    }

    pub fn set_font_metrics(&mut self, font_size: f32, line_height: f32, cx: &mut Context<Self>) {
        let font_size = px(font_size.clamp(8., 32.));
        let line_height = px(line_height.clamp(f32::from(font_size), 56.));
        if self.font_size != font_size {
            self.font_metrics = None;
        }
        if (self.font_size, self.line_height) != (font_size, line_height) {
            self.font_size = font_size;
            self.line_height = line_height;
            cx.notify();
        }
    }

    pub fn set_font_family(&mut self, family: Option<&str>, cx: &mut Context<Self>) {
        let family = family.and_then(|family| {
            cx.text_system()
                .all_font_names()
                .into_iter()
                .find(|installed| installed.eq_ignore_ascii_case(family))
        });
        if self.font_family != family {
            self.font_family = family;
            self.font_metrics = None;
            self.geometry = None;
            cx.notify();
        }
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn prompt_serial(&self) -> u64 {
        self.prompt_serial
    }

    pub fn prompt_text(&self) -> Option<&str> {
        self.prompt.as_ref().map(|(_, text)| text.as_str())
    }

    pub fn notification_serial(&self) -> u64 {
        self.notification_serial
    }

    pub fn notification(&self) -> Option<&str> {
        self.notification.as_deref()
    }

    pub fn agent_name(&self) -> Option<&'static str> {
        self.title_agent.or(self.launched_agent)
    }

    pub fn agent_elapsed(&self) -> Option<Duration> {
        self.agent_started.map(|started| started.elapsed())
    }

    pub fn fullscreen_name(&self) -> Option<&str> {
        self.fullscreen
            .as_ref()
            .map(|(_, name)| name.as_deref().unwrap_or("全屏程序"))
    }

    pub fn system_message(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.timeline
            .system_after_output(Local::now(), message.into());
        cx.notify();
    }

    pub fn formatted_log(&self) -> String {
        self.primary_log.clone().unwrap_or_else(|| {
            self.timeline
                .formatted_log(self.term.grid(), self.timestamp_format)
        })
    }

    pub fn end_session(&mut self, cx: &mut Context<Self>) {
        let now = Local::now();
        self.sync_flush = None;
        if self.parser.sync_timeout().sync_timeout().is_some() {
            self.finish_sync(now);
        }
        if self.mode().contains(TermMode::ALT_SCREEN) {
            self.advance(b"\x1b[?1049l", now);
        }
        self.output_changed(now, cx);
        self.launched_agent = None;
        self.title_agent = None;
        self.agent_started = None;
        self.clipboard_request = None;
        cx.notify();
    }

    pub fn set_light(&mut self, light: bool, cx: &mut Context<Self>) {
        if self.light != light {
            self.light = light;
            cx.notify();
        }
    }

    pub fn set_compact(&mut self, compact: bool, cx: &mut Context<Self>) {
        if self.compact != compact {
            self.compact = compact;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    pub fn feed(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        let time = Local::now();
        let mut offset = 0;
        for (end, marker) in self.scanner.scan(bytes) {
            self.advance(&bytes[offset..end], time);
            if self.parser.sync_timeout().sync_timeout().is_some() {
                if self.sync_markers.len() < 256 {
                    self.sync_markers.push(marker);
                }
            } else {
                self.apply_marker(marker, time);
            }
            offset = end;
        }
        self.advance(&bytes[offset..], time);
        self.update_sync_timeout(cx);
        self.output_changed(time, cx);
    }

    fn advance(&mut self, bytes: &[u8], now: DateTime<Local>) {
        let inline_agent = self.agent_name().is_some();
        self.parser.advance(
            &mut ParserHandler {
                term: &mut self.term,
                keyboard: &mut self.keyboard,
                events: &self.events,
                tracking: Some(Tracking {
                    timeline: &mut self.timeline,
                    reflow: &mut self.primary_snapshot,
                    primary_log: &mut self.primary_log,
                    timestamp_format: self.timestamp_format,
                    reflowed: &mut self.primary_reflowed,
                    redraw: &mut self.redraw,
                    fullscreen: &mut self.fullscreen,
                    program: self.last_program.as_deref(),
                    inline_agent,
                    now,
                }),
            },
            bytes,
        );
        self.handle_terminal_events();
        if self.parser.sync_timeout().sync_timeout().is_none() {
            for marker in std::mem::take(&mut self.sync_markers) {
                self.apply_marker(marker, now);
            }
        }
    }

    fn apply_marker(&mut self, marker: Marker, now: DateTime<Local>) {
        match marker {
            Marker::Directory(path) => self.cwd = Some(path),
            Marker::Notification(message) => self.push_notification(message),
            marker if !self.mode().contains(TermMode::ALT_SCREEN) => match marker {
                Marker::Prompt => self.timeline.prompt(self.term.grid(), now),
                Marker::PromptEnd => {
                    self.timeline.end_prompt(self.term.grid());
                    self.update_prompt(true);
                }
                Marker::Output => self.timeline.start_command(now),
                Marker::Done(status) => self.timeline.finish_command(self.term.grid(), now, status),
                _ => {}
            },
            _ => {}
        }
    }

    fn push_notification(&mut self, message: String) {
        self.notification = Some(if message.is_empty() {
            "终端程序发出提醒".into()
        } else {
            message
        });
        self.notification_serial = self.notification_serial.wrapping_add(1);
    }

    fn update_sync_timeout(&mut self, cx: &mut Context<Self>) {
        let Some(deadline) = self.parser.sync_timeout().sync_timeout() else {
            self.sync_flush = None;
            return;
        };
        if self
            .sync_flush
            .as_ref()
            .is_some_and(|(pending, _)| *pending == deadline)
        {
            return;
        }
        let timer = cx
            .background_executor()
            .timer(deadline.saturating_duration_since(Instant::now()));
        let task = cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.parser.sync_timeout().sync_timeout() == Some(deadline) {
                    let now = Local::now();
                    view.finish_sync(now);
                    view.output_changed(now, cx);
                }
            });
        });
        self.sync_flush = Some((deadline, task));
    }

    fn finish_sync(&mut self, now: DateTime<Local>) {
        let inline_agent = self.agent_name().is_some();
        self.parser.stop_sync(&mut ParserHandler {
            term: &mut self.term,
            keyboard: &mut self.keyboard,
            events: &self.events,
            tracking: Some(Tracking {
                timeline: &mut self.timeline,
                reflow: &mut self.primary_snapshot,
                primary_log: &mut self.primary_log,
                timestamp_format: self.timestamp_format,
                reflowed: &mut self.primary_reflowed,
                redraw: &mut self.redraw,
                fullscreen: &mut self.fullscreen,
                program: self.last_program.as_deref(),
                inline_agent,
                now,
            }),
        });
        for marker in std::mem::take(&mut self.sync_markers) {
            self.apply_marker(marker, now);
        }
    }

    fn output_changed(&mut self, now: DateTime<Local>, cx: &mut Context<Self>) {
        self.handle_terminal_events();
        if !self.mode().contains(TermMode::ALT_SCREEN) {
            self.timeline.output(self.term.grid(), now);
            self.update_prompt(false);
        }
        self.schedule_search(false, cx);
        cx.notify();
    }

    fn update_prompt(&mut self, prompt_complete: bool) {
        let Some(number) = self.timeline.latest_prompt() else {
            return;
        };
        if self.prompt.as_ref().is_some_and(|(old, _)| *old == number) {
            return;
        }
        let grid = self.term.grid();
        let line = grid.cursor.point.line;
        if self
            .timeline
            .stamp(grid, line)
            .is_some_and(|stamp| stamp.number == number)
        {
            let text = timeline::row_text(grid, line);
            if !text.is_empty()
                && (prompt_complete || text.trim_end().ends_with(['$', '#', '❯', '%', '>']))
            {
                self.prompt_serial = self.prompt_serial.wrapping_add(1);
                let previous = self.agent_name();
                self.launched_agent = None;
                if prompt_complete || timeline::is_powershell_prompt(&text) {
                    self.title_agent = None;
                    self.last_program = None;
                }
                self.prompt = Some((number, text));
                self.track_agent_change(previous);
            }
        }
    }

    pub fn send_draft(&mut self, text: &str, execute: bool, cx: &mut Context<Self>) {
        let bytes = if execute {
            self.timeline.start_fallback(Local::now());
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            let program = text
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().next())
                .map(str::to_owned);
            self.program_launched(program, cx);
            let mut bytes = Vec::new();
            for line in text.trim_end_matches('\n').split('\n') {
                bytes.extend(input::paste(
                    line,
                    self.mode().contains(TermMode::BRACKETED_PASTE),
                ));
                bytes.push(b'\r');
            }
            bytes
        } else {
            input::paste(text, self.mode().contains(TermMode::BRACKETED_PASTE))
        };
        self.send_input(bytes, cx);
    }

    fn paste_clipboard(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        let text = item.text();
        if (text.is_none() || self.agent_name().is_some())
            && let Some(image) = item.into_entries().find_map(|entry| match entry {
                ClipboardEntry::Image(image) if !image.bytes.is_empty() => Some(image),
                _ => None,
            })
        {
            cx.emit(ClipboardImage {
                bytes: image.bytes,
                extension: image.format.extension(),
            });
        } else if let Some(text) = text {
            self.send_draft(&text, false, cx);
        }
    }

    fn program_launched(&mut self, program: Option<String>, cx: &mut Context<Self>) {
        let previous = self.agent_name();
        self.launched_agent = program
            .as_deref()
            .and_then(|program| program.rsplit(['/', '\\']).next())
            .and_then(|program| agent_label(program.trim_end_matches(".exe")));
        self.last_program = program;
        self.track_agent_change(previous);
        if previous != self.agent_name() {
            cx.notify();
        }
    }

    fn track_agent_change(&mut self, previous: Option<&'static str>) {
        if previous != self.agent_name() {
            self.agent_started = self.agent_name().map(|_| Instant::now());
        }
    }

    fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if self.term.grid().display_offset() > 0
            || self.timeline_offset > 0
            || self.term.selection.take().is_some()
        {
            self.term.scroll_display(Scroll::Bottom);
            self.timeline_offset = 0;
            self.term.selection = None;
            cx.notify();
        }
        (self.send)(TerminalCommand::Data(bytes));
    }

    fn handle_terminal_events(&mut self) {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        let size = self.size();
        for event in events {
            let reply = match event {
                Event::PtyWrite(reply) => reply,
                Event::ColorRequest(index, reply) => reply(color_components(
                    Palette::new(self.light).indexed(index, self.term.colors()),
                )),
                Event::TextAreaSizeRequest(reply) => reply(WindowSize {
                    num_lines: size.rows,
                    num_cols: size.cols,
                    cell_width: size.pixel_width / size.cols.max(1),
                    cell_height: size.pixel_height / size.rows.max(1),
                }),
                Event::Title(title) => {
                    let previous = self.agent_name();
                    self.title_agent = title
                        .split(|character: char| !character.is_ascii_alphanumeric())
                        .find_map(agent_label);
                    self.title = Some(
                        title
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(240)
                            .collect(),
                    );
                    self.track_agent_change(previous);
                    continue;
                }
                Event::ResetTitle => {
                    let previous = self.agent_name();
                    self.title = None;
                    self.title_agent = None;
                    self.track_agent_change(previous);
                    continue;
                }
                Event::Bell => {
                    self.push_notification(String::new());
                    continue;
                }
                Event::ClipboardStore(_, text) => {
                    self.clipboard_request = Some(text);
                    continue;
                }
                _ => continue,
            };
            (self.send)(TerminalCommand::Data(reply.into_bytes()));
        }
    }

    fn apply_geometry(&mut self, geometry: Geometry, cx: &mut Context<Self>) {
        self.geometry = Some(geometry);
        let size = geometry.size;
        if self.size == size {
            return;
        }
        let grid_changed = self.size.cols != size.cols || self.size.rows != size.rows;
        self.size = size;
        if grid_changed {
            let alternate = self.mode().contains(TermMode::ALT_SCREEN);
            let logical = (!alternate).then(|| self.timeline.before_resize(self.term.grid()));
            self.term.resize(size);
            if alternate {
                self.primary_reflowed = true;
            } else if let Some(logical) = logical {
                self.timeline.resized(self.term.grid(), logical);
            }
            self.schedule_search(false, cx);
        }
        let timer = cx.background_executor().timer(Duration::from_millis(100));
        self.resize_pending = Some(cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, _| {
                (view.send)(TerminalCommand::Resize(view.size))
            });
        }));
        cx.notify();
    }

    fn on_focus(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.term.is_focused = focused;
        if !focused {
            self.preedit.clear();
        }
        if self.mode().contains(TermMode::FOCUS_IN_OUT) {
            let message = if focused { b"\x1b[I" } else { b"\x1b[O" };
            (self.send)(TerminalCommand::Data(message.to_vec()));
        }
        cx.notify();
    }

    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索终端输出"));
            let subscription =
                cx.subscribe_in(&input, window, |view, _, event, _, cx| match event {
                    InputEvent::Change => view.schedule_search(true, cx),
                    InputEvent::PressEnter { shift, .. } => view.next_hit(*shift, cx),
                    _ => {}
                });
            self.search.input = Some((input, subscription));
        }
        self.search.open = true;
        if let Some((input, _)) = &self.search.input {
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.schedule_search(true, cx);
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.open = false;
        self.search.pending = None;
        self.search.matches.clear();
        self.focus(window, cx);
        cx.notify();
    }

    fn schedule_search(&mut self, reset: bool, cx: &mut Context<Self>) {
        if !self.search.open || (!reset && self.search.pending.is_some()) {
            return;
        }
        let Some((input, _)) = &self.search.input else {
            return;
        };
        let query = input.read(cx).value();
        if query.is_empty() {
            self.search.pending = None;
            self.search.update(&self.term, "", true);
            cx.notify();
            return;
        }
        let timer = cx.background_executor().timer(Duration::from_millis(140));
        self.search.pending = Some(cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                view.search.update(&view.term, &query, reset);
                if reset {
                    view.reveal_hit();
                }
                view.search.pending.take().map(Task::detach);
                cx.notify();
            });
        }));
    }

    fn reveal_hit(&mut self) {
        if let Some(hit) = self.search.matches.get(self.search.current) {
            let line = hit.start().line;
            if let Some(stamp) = self.timeline.stamp(self.term.grid(), line)
                && let Some((start, _)) = self.timeline.command_for(stamp.number)
                && let Some(block) = self.timeline.commands.get_mut(&start)
            {
                block.collapsed = false;
            }
            let offset = self.term.grid().display_offset() as i32;
            let top = -offset;
            let bottom = top + self.term.screen_lines() as i32 - 1;
            if !(top..=bottom).contains(&line.0) {
                let target = (-line.0 + self.term.screen_lines() as i32 / 3).max(0);
                self.term.scroll_display(Scroll::Delta(target - offset));
                self.timeline_offset = 0;
            } else if self.timeline_overflow > 0 && !self.line_map.contains(&Some(line)) {
                self.timeline_offset = if self
                    .line_map
                    .iter()
                    .flatten()
                    .next()
                    .is_some_and(|first| line < *first)
                {
                    self.timeline_overflow
                } else {
                    0
                };
            }
        }
    }

    fn next_hit(&mut self, previous: bool, cx: &mut Context<Self>) {
        let count = self.search.matches.len();
        if count != 0 {
            self.search.current =
                (self.search.current + if previous { count - 1 } else { 1 }) % count;
            self.reveal_hit();
            cx.notify();
        }
    }

    fn render_search(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let Some((input, _)) = &self.search.input else {
            return div();
        };
        let option = |id, label: &'static str, active| {
            div()
                .id(id)
                .w(px(24.))
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(12.))
                .bg(rgb(if active {
                    palette.active()
                } else {
                    palette.raised()
                }))
                .text_color(rgb(if active {
                    palette.accent()
                } else {
                    palette.secondary()
                }))
                .hover(|this| this.bg(rgb(palette.active())))
                .child(label)
        };
        div()
            .absolute()
            .top(px(8.))
            .right(px(26.))
            .flex()
            .items_center()
            .gap(px(4.))
            .max_w_full()
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(palette.border()))
            .bg(rgb(palette.raised()))
            .shadow_md()
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                if key.key == "escape" {
                    view.close_search(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                Input::new(input)
                    .aria_label("搜索终端输出")
                    .w(px(220.))
                    .min_w_0()
                    .h(px(26.))
                    .rounded(px(6.)),
            )
            .child(
                option("terminal-search-case", "Aa", self.search.case_sensitive).on_click(
                    cx.listener(|view, _, _, cx| {
                        view.search.case_sensitive = !view.search.case_sensitive;
                        view.schedule_search(true, cx);
                    }),
                ),
            )
            .child(
                option("terminal-search-regex", ".*", self.search.regex).on_click(cx.listener(
                    |view, _, _, cx| {
                        view.search.regex = !view.search.regex;
                        view.schedule_search(true, cx);
                    },
                )),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(6.))
                    .text_size(px(11.5))
                    .text_color(rgb(palette.secondary()))
                    .child(self.search.label()),
            )
            .child(
                option("terminal-search-up", "↑", false)
                    .on_click(cx.listener(|view, _, _, cx| view.next_hit(true, cx))),
            )
            .child(
                option("terminal-search-down", "↓", false)
                    .on_click(cx.listener(|view, _, _, cx| view.next_hit(false, cx))),
            )
            .child(
                option("terminal-search-close", "×", false)
                    .on_click(cx.listener(|view, _, window, cx| view.close_search(window, cx))),
            )
    }

    fn render_clipboard_request(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        div()
            .absolute()
            .bottom(px(10.))
            .right(px(26.))
            .flex()
            .items_center()
            .gap(px(8.))
            .p(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(palette.border()))
            .bg(rgb(palette.raised()))
            .shadow_md()
            .text_size(px(12.))
            .text_color(rgb(palette.text()))
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child("远端程序请求写入本机剪贴板")
            .child(
                div()
                    .id("terminal-clipboard-allow")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(palette.active()))
                    .cursor_pointer()
                    .text_color(rgb(palette.accent()))
                    .child("允许复制")
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(text) = view.clipboard_request.take() {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("terminal-clipboard-reject")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .child("取消")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.clipboard_request = None;
                        cx.notify();
                    })),
            )
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let m = key.modifiers;
        if self.search.open
            && self
                .search
                .input
                .as_ref()
                .is_some_and(|(input, _)| input.read(cx).focus_handle(cx).is_focused(window))
        {
            return;
        }
        let clipboard_shortcut = m.platform || (m.control && m.shift && !m.alt);
        let copied = (key.key == "c" && (clipboard_shortcut || (m.control && !m.shift && !m.alt)))
            .then(|| self.term.selection_to_string())
            .flatten();
        let handled = match key.key.as_str() {
            "f" if clipboard_shortcut => {
                self.open_search(window, cx);
                true
            }
            "c" if clipboard_shortcut || copied.is_some() => {
                if let Some(text) = copied {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                true
            }
            "v" if clipboard_shortcut => {
                if let Some(item) = cx.read_from_clipboard() {
                    self.paste_clipboard(item, cx);
                }
                true
            }
            "pageup" | "pagedown" if m.shift && !m.control && !m.alt && !m.platform => {
                let page = self.term.screen_lines() as i32;
                self.scroll_timeline(if key.key == "pageup" { page } else { -page });
                cx.notify();
                true
            }
            _ => {
                if key.key == "enter"
                    && !m.alt
                    && !m.shift
                    && !m.control
                    && !m.platform
                    && !self.raw()
                {
                    self.timeline.start_fallback(Local::now());
                    if let Some(prompt) = self.prompt_text() {
                        let grid = self.term.grid();
                        let text = timeline::row_text(grid, grid.cursor.point.line);
                        let program = text
                            .strip_prefix(prompt)
                            .and_then(|text| text.split_whitespace().next())
                            .map(str::to_owned);
                        if program.is_some() {
                            self.program_launched(program, cx);
                        }
                    }
                }
                self.send_key(key, cx)
            }
        };
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn send_key(&mut self, key: &Keystroke, cx: &mut Context<Self>) -> bool {
        let mode = self.mode();
        if let Some(bytes) = input::key_bytes(key, mode | self.keyboard.mode(mode)) {
            self.send_input(bytes, cx);
            true
        } else {
            false
        }
    }

    fn send_tab(&mut self, shift: bool, cx: &mut Context<Self>) {
        self.send_key(
            &Keystroke {
                key: "tab".into(),
                key_char: None,
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
            },
            cx,
        );
    }

    fn mouse_reporting(&self, modifiers: Modifiers) -> bool {
        !modifiers.shift
            && self.mode().intersects(TermMode::MOUSE_MODE)
            && self.term.grid().display_offset() == 0
            && self.timeline_offset == 0
    }

    fn report_mouse(
        &self,
        button: u8,
        position: (usize, usize),
        release: bool,
        modifiers: Modifiers,
    ) {
        let (column, row) = position;
        let Some(line) = self.selection_point(column, row, release) else {
            return;
        };
        let Ok(row) = usize::try_from(line.line.0) else {
            return;
        };
        if let Some(bytes) =
            input::mouse_bytes(self.mode(), button, column, row, release, modifiers)
        {
            (self.send)(TerminalCommand::Data(bytes));
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window, cx);
        let Some(geometry) = self.geometry else {
            return;
        };
        let alternate = self.mode().contains(TermMode::ALT_SCREEN);
        if event.button == MouseButton::Left
            && !alternate
            && geometry.ruler_contains(event.position)
        {
            let grid = self.term.grid();
            let fraction =
                (event.position.y - geometry.bounds.origin.y) / geometry.bounds.size.height;
            if grid.history_size() == 0 {
                self.timeline_offset =
                    ((1. - fraction) * self.timeline_overflow as f32).round() as usize;
                cx.notify();
                return;
            }
            let target = (fraction * grid.total_lines() as f32) as i32 - grid.history_size() as i32;
            let target = (-target + grid.screen_lines() as i32 / 2).max(0);
            self.term
                .scroll_display(Scroll::Delta(target - grid.display_offset() as i32));
            self.timeline_offset = 0;
            cx.notify();
            return;
        }
        if event.button == MouseButton::Left
            && !alternate
            && (geometry.grid.origin.x - geometry.marker_width..geometry.grid.origin.x)
                .contains(&event.position.x)
        {
            let y = (event.position.y - geometry.grid.origin.y) / geometry.line_height;
            if y >= 0.
                && let Some(Some(line)) = self.line_map.get(y as usize)
                && let Some(stamp) = self.timeline.stamp(self.term.grid(), *line)
                && let Some(block) = self.timeline.commands.get_mut(&stamp.number)
                && block.last_line.is_some_and(|end| end > stamp.number)
            {
                block.collapsed = !block.collapsed;
                cx.notify();
                return;
            }
        }
        let Some((column, row, side)) = self
            .geometry
            .and_then(|geometry| geometry.locate(event.position, false))
        else {
            return;
        };
        let point = self.selection_point(column, row, false);
        if event.button == MouseButton::Left
            && (event.modifiers.control || event.modifiers.platform)
            && let Some(url) = point.and_then(|point| self.url_at(point))
        {
            cx.open_url(&url);
            return;
        }
        if self.mouse_reporting(event.modifiers) {
            if point.is_none() {
                return;
            }
            if let Some(button) = input::mouse_button(event.button) {
                self.reported_button = Some(event.button);
                self.last_mouse_cell = Some((column, row));
                self.report_mouse(button, (column, row), false, event.modifiers);
            }
        } else if event.button == MouseButton::Left {
            let Some(point) = point else {
                return;
            };
            let ty = match (event.modifiers.alt, event.click_count) {
                (true, _) => SelectionType::Block,
                (false, 2) => SelectionType::Semantic,
                (false, 3..) => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            self.term.selection = Some(Selection::new(ty, point, side));
            self.selecting = true;
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent) {
        if self.reported_button == Some(event.button) {
            self.reported_button = None;
            if let Some((column, row, _)) = self
                .geometry
                .and_then(|geometry| geometry.locate(event.position, true))
                && let Some(button) = input::mouse_button(event.button)
            {
                self.report_mouse(button, (column, row), true, event.modifiers);
            }
        }
        if event.button == MouseButton::Left {
            self.selecting = false;
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((column, row, side)) = self
            .geometry
            .and_then(|geometry| geometry.locate(event.position, self.selecting))
        else {
            return;
        };
        if self.selecting && event.dragging() {
            if let Some(point) = self.selection_point(column, row, true)
                && let Some(selection) = &mut self.term.selection
            {
                selection.update(point, side);
                cx.notify();
            }
            return;
        }
        let mode = self.mode();
        if self.mouse_reporting(event.modifiers)
            && (mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && self.reported_button.is_some()))
            && self.last_mouse_cell != Some((column, row))
        {
            self.last_mouse_cell = Some((column, row));
            let button = self
                .reported_button
                .and_then(input::mouse_button)
                .unwrap_or(3);
            self.report_mouse(button + 32, (column, row), false, event.modifiers);
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let Some(geometry) = self.geometry else {
            return;
        };
        self.wheel_remainder +=
            event.delta.pixel_delta(geometry.line_height).y / geometry.line_height;
        let count = self.wheel_remainder.trunc().clamp(-100., 100.) as i32;
        self.wheel_remainder -= count as f32;
        if count != 0 {
            if self.mouse_reporting(event.modifiers) {
                if let Some((column, row, _)) = geometry.locate(event.position, false) {
                    let button = if count > 0 { 64 } else { 65 };
                    for _ in 0..count.unsigned_abs() {
                        self.report_mouse(button, (column, row), false, event.modifiers);
                    }
                }
            } else if !event.modifiers.shift
                && self
                    .mode()
                    .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            {
                let key = if count > 0 { b"\x1bOA" } else { b"\x1bOB" };
                (self.send)(TerminalCommand::Data(
                    key.repeat(count.unsigned_abs() as usize),
                ));
            } else {
                self.scroll_timeline(count);
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    fn scroll_timeline(&mut self, mut delta: i32) {
        let physical = self.term.grid().display_offset() as i32;
        if delta > 0 && physical == 0 {
            let available = self.timeline_overflow.saturating_sub(self.timeline_offset);
            let consumed = available.min(delta as usize);
            self.timeline_offset += consumed;
            delta -= consumed as i32;
        } else if delta < 0 && -delta > physical {
            let consumed = self.timeline_offset.min((-delta - physical) as usize);
            self.timeline_offset -= consumed;
            delta += consumed as i32;
        }
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    fn selection_point(&self, column: usize, row: usize, clamp: bool) -> Option<CellPoint> {
        let line = self.line_map.get(row).copied().flatten().or_else(|| {
            if !clamp {
                return None;
            }
            self.line_map[..row.min(self.line_map.len())]
                .iter()
                .rev()
                .find_map(|line| *line)
                .or_else(|| self.line_map.iter().find_map(|line| *line))
        })?;
        Some(CellPoint::new(line, Column(column)))
    }

    fn url_at(&self, point: CellPoint) -> Option<String> {
        let grid = self.term.grid();
        let row = &grid[point.line];
        let allowed = |url: &str| {
            url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:")
        };
        if let Some(link) = row[point.column].hyperlink() {
            return allowed(link.uri()).then(|| link.uri().to_owned());
        }
        let boundary = |column: usize| {
            let c = row[Column(column)].c;
            c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '│')
        };
        if boundary(point.column.0) {
            return None;
        }
        let start = (0..point.column.0)
            .rev()
            .find(|&column| boundary(column))
            .map_or(0, |index| index + 1);
        let end = (point.column.0..grid.columns())
            .find(|&column| boundary(column))
            .unwrap_or(grid.columns());
        let url: String = (start..end).map(|column| row[Column(column)].c).collect();
        let url = url.trim_end_matches(['.', ',', ';', '!', '?']);
        allowed(url).then(|| url.to_owned())
    }

    fn copy_formatted(&self, cx: &mut App) {
        let Some(selection) = self.term.renderable_content().selection else {
            return;
        };
        let grid = self.term.grid();
        let mut result = String::new();
        let mut previous = None;
        for line in (selection.start.line.0..=selection.end.line.0).map(Line) {
            if let Some(stamp) = self.timeline.stamp(grid, line)
                && previous != Some(stamp.number)
            {
                result.push_str(&format!(
                    "{} {}  ",
                    stamp.number,
                    self.timeline.timestamp(stamp.time, self.timestamp_format)
                ));
                previous = Some(stamp.number);
            }
            let start = if selection.is_block || selection.start.line == line {
                selection.start.column
            } else {
                Column(0)
            };
            let end = if selection.is_block || selection.end.line == line {
                selection.end.column
            } else {
                grid.last_column()
            };
            let text = self
                .term
                .bounds_to_string(CellPoint::new(line, start), CellPoint::new(line, end));
            result.push_str(text.trim_end_matches('\n'));
            if selection.is_block || !timeline::wraps(grid, line) {
                result.push('\n');
            }
        }
        cx.write_to_clipboard(ClipboardItem::new_string(result));
    }

    pub fn export_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_new_path(
            &dirs::download_dir().unwrap_or_else(std::env::temp_dir),
            Some("capterm-session.log"),
        );
        let log = self.formatted_log();
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |view, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let result = executor
                .spawn(async move { std::fs::write(&path, log).map(|_| path) })
                .await;
            let _ = view.update_in(cx, |view, _, cx| {
                let message = match result {
                    Ok(path) => format!("会话日志已导出到 {}", path.display()),
                    Err(error) => format!("导出会话日志失败（{error}）"),
                };
                view.system_message(message, cx);
            });
        })
        .detach();
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 1 {
        format!("{:.1} 秒", duration.as_secs_f64())
    } else if seconds < 60 {
        format!("{seconds} 秒")
    } else if seconds < 3600 {
        format!("{} 分 {} 秒", seconds / 60, seconds % 60)
    } else {
        format!("{} 小时 {} 分", seconds / 3600, seconds / 60 % 60)
    }
}

fn agent_label(program: &str) -> Option<&'static str> {
    [
        ("claude", "Claude Code"),
        ("codex", "Codex"),
        ("gemini", "Gemini CLI"),
        ("opencode", "OpenCode"),
        ("aider", "Aider"),
    ]
    .into_iter()
    .find_map(|(command, label)| program.eq_ignore_ascii_case(command).then_some(label))
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_subscriptions.is_none() {
            self.focus_subscriptions = Some([
                cx.on_focus(&self.focus, window, |view, _, cx| view.on_focus(true, cx)),
                cx.on_blur(&self.focus, window, |view, _, cx| view.on_focus(false, cx)),
            ]);
        }
        let entity = cx.entity();
        let input_entity = entity.clone();
        let context = entity.downgrade();
        let focus = self.focus.clone();
        let surface = canvas(
            move |bounds, window, cx| {
                entity.update(cx, |view, cx| {
                    let text_system = window.text_system();
                    let font_size = view.font_size;
                    let family = view.font_family.as_deref();
                    let (font, cell_width) = view
                        .font_metrics
                        .get_or_insert_with(|| {
                            let font = terminal_font(text_system, family);
                            let font_id = text_system.resolve_font(&font);
                            let width = text_system
                                .ch_advance(font_id, font_size)
                                .unwrap_or(px(8.))
                                .max(px(1.));
                            (font, width)
                        })
                        .clone();
                    let alternate = view.mode().contains(TermMode::ALT_SCREEN);
                    let geometry = Geometry::new(
                        bounds,
                        cell_width,
                        view.show_numbers && !view.manual_raw,
                        view.show_time && !view.manual_raw,
                        view.timestamp_format,
                        view.compact,
                        alternate,
                        font_size,
                        view.line_height,
                    );
                    view.apply_geometry(geometry, cx);
                    let frame = Frame::new(
                        &view.term,
                        &view.timeline,
                        geometry,
                        Palette::new(view.light),
                        font,
                        view.preedit.clone(),
                        FrameOptions {
                            time: view.timestamp_format,
                            idle: view.idle_threshold,
                            dim_repeated: view.dim_repeated,
                            timeline_offset: view.timeline_offset,
                            show_timeline: !view.manual_raw,
                            linux_highlights: view.linux_highlights.as_deref(),
                            matches: &view.search.matches,
                            current: view.search.current,
                        },
                    );
                    view.line_map.clone_from(&frame.line_map);
                    if view.term.grid().display_offset() == 0 {
                        view.timeline_overflow = frame.timeline_overflow;
                        view.timeline_offset = view.timeline_offset.min(frame.timeline_overflow);
                    }
                    frame
                })
            },
            move |bounds, frame, window, cx| {
                window.handle_input(
                    &focus,
                    ElementInputHandler::new(frame.geometry.grid, input_entity),
                    cx,
                );
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    frame.paint(window, cx)
                });
            },
        )
        .size_full();

        let palette = Palette::new(self.light);
        let mut viewport = div()
            .id("terminal-input-surface")
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .size_full()
            .bg(rgb(palette.background()))
            .on_action(cx.listener(|view, _: &TerminalTab, _, cx| view.send_tab(false, cx)))
            .on_action(cx.listener(|view, _: &TerminalBackTab, _, cx| view.send_tab(true, cx)))
            .on_key_down(cx.listener(Self::key_down))
            .on_any_mouse_down(cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(|view, event, _, cx| view.mouse_move(event, cx)))
            .on_scroll(cx.listener(|view, event, _, cx| view.scroll(event, cx)))
            .child(surface);
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            viewport = viewport
                .on_mouse_up(
                    button,
                    cx.listener(|view, event, _, _| view.mouse_up(event)),
                )
                .on_mouse_up_out(
                    button,
                    cx.listener(|view, event, _, _| view.mouse_up(event)),
                );
        }
        // Keep overlays outside the terminal key context so their controls can navigate with Tab.
        let mut viewport = div()
            .id("terminal-surface")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(viewport);
        if self.search.open {
            viewport = viewport.child(self.render_search(palette, cx));
        }
        if self.clipboard_request.is_some() {
            viewport = viewport.child(self.render_clipboard_request(palette, cx));
        }
        if self.mode().intersects(TermMode::MOUSE_MODE) {
            return viewport.into_any_element();
        }
        let selection = self.term.selection_to_string().is_some();
        viewport
            .context_menu(move |menu, _, _| {
                let copy = context.clone();
                let formatted = context.clone();
                let paste = context.clone();
                let export = context.clone();
                menu.item(PopupMenuItem::new("复制").disabled(!selection).on_click(
                    move |_, _, cx| {
                        let _ = copy.update(cx, |view, cx| {
                            if let Some(text) = view.term.selection_to_string() {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        });
                    },
                ))
                .item(
                    PopupMenuItem::new("复制（含行号与时间）")
                        .disabled(!selection)
                        .on_click(move |_, _, cx| {
                            let _ = formatted.update(cx, |view, cx| view.copy_formatted(cx));
                        }),
                )
                .item(PopupMenuItem::new("粘贴").on_click(move |_, _, cx| {
                    let _ = paste.update(cx, |view, cx| {
                        if let Some(item) = cx.read_from_clipboard() {
                            view.paste_clipboard(item, cx);
                        }
                    });
                }))
                .separator()
                .item(
                    PopupMenuItem::new("导出会话日志…").on_click(move |_, window, cx| {
                        let _ = export.update(cx, |view, cx| view.export_log(window, cx));
                    }),
                )
            })
            .into_any_element()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let utf16: Vec<_> = self.preedit.encode_utf16().collect();
        Some(String::from_utf16_lossy(utf16.get(range)?))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.preedit.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.preedit.is_empty()).then(|| 0..self.preedit.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit.clear();
        cx.notify();
    }

    fn paste(&mut self, item: ClipboardItem, _: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard(item, cx);
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit.clear();
        self.send_input(text.as_bytes().to_vec(), cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = text.to_owned();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry?;
        let cursor = self.term.grid().cursor.point;
        let row = self
            .line_map
            .iter()
            .position(|line| *line == Some(cursor.line))?;
        let origin = point(
            element.origin.x + geometry.cell_width * cursor.column.0 as f32,
            element.origin.y + geometry.line_height * row as f32,
        );
        Some(Bounds::new(
            origin,
            size(geometry.cell_width, geometry.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
