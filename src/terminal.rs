//! A native VT viewport driven by raw bytes from an interactive SSH PTY.

mod input;
mod paint;
mod parser;
mod timeline;

use std::{cell::RefCell, ops::Range, rc::Rc, sync::Arc, time::Instant};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as CellPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, Osc52, TermMode},
    vte::ansi::Processor,
};
use chrono::Local;
use gpui_kit::{
    App, Bounds, ClipboardItem, ContentMask, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, Font, InteractiveElement, IntoElement, KeyDownEvent, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollEvent, Styled, Subscription, Task, UTF16Selection, Window, canvas, div, point, px, rgb,
    size,
};

use paint::{Frame, Palette, color_components, terminal_font};
use parser::{KeyboardProtocol, ParserHandler};
use timeline::Timeline;

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
    cell_width: Pixels,
    line_height: Pixels,
    size: TerminalSize,
}

impl Geometry {
    fn new(
        bounds: Bounds<Pixels>,
        cell_width: Pixels,
        show_gutter: bool,
        compact: bool,
        raw: bool,
    ) -> Self {
        let (number_width, time_width) = match (show_gutter, compact) {
            (true, true) => (px(36.), px(60.)),
            (true, false) => (px(51.), px(72.)),
            (false, _) => (px(0.), px(0.)),
        };
        let horizontal_padding = if raw {
            px(18.)
        } else if compact {
            px(9.)
        } else {
            px(14.)
        };
        let top_padding = if raw { px(17.) } else { px(13.) };
        let width =
            (bounds.size.width - number_width - time_width - horizontal_padding * 2.).max(px(0.));
        let height = (bounds.size.height - top_padding).max(px(0.));
        let line_height = px(22.);
        let cols = (width / cell_width).floor().clamp(2., 1000.) as u16;
        let rows = (height / line_height).floor().clamp(1., 1000.) as u16;
        let size_px = size(cell_width * f32::from(cols), line_height * f32::from(rows));
        let grid = Bounds::new(
            point(
                bounds.origin.x + number_width + time_width + horizontal_padding,
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
            cell_width,
            line_height,
            size,
        }
    }

    fn gutter_width(self) -> Pixels {
        self.number_width + self.time_width
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
    timeline: Timeline,
    primary_reflowed: bool,
    font_metrics: Option<(Font, Pixels)>,
    focus: FocusHandle,
    focus_subscriptions: Option<[Subscription; 2]>,
    preedit: String,
    selecting: bool,
    reported_button: Option<MouseButton>,
    last_mouse_cell: Option<(usize, usize)>,
    wheel_remainder: f32,
    manual_raw: bool,
    show_gutter: bool,
    compact: bool,
    light: bool,
}

impl TerminalView {
    pub fn new(send: impl Fn(TerminalCommand) + 'static, cx: &mut Context<Self>) -> Self {
        let config = Config {
            osc52: Osc52::Disabled,
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
            timeline: Timeline::default(),
            primary_reflowed: false,
            font_metrics: None,
            focus: cx.focus_handle(),
            focus_subscriptions: None,
            preedit: String::new(),
            selecting: false,
            reported_button: None,
            last_mouse_cell: None,
            wheel_remainder: 0.,
            manual_raw: false,
            show_gutter: true,
            compact: false,
            light: false,
        }
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn raw(&self) -> bool {
        self.manual_raw || self.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn set_raw(&mut self, raw: bool, cx: &mut Context<Self>) {
        if self.manual_raw != raw {
            self.manual_raw = raw;
            if raw {
                self.term.scroll_display(Scroll::Bottom);
            }
            cx.notify();
        }
    }

    pub fn set_gutter(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_gutter != show {
            self.show_gutter = show;
            cx.notify();
        }
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
        self.parser.advance(
            &mut ParserHandler {
                term: &mut self.term,
                keyboard: &mut self.keyboard,
                events: &self.events,
            },
            bytes,
        );
        self.update_sync_timeout(cx);
        self.output_changed(cx);
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
                    view.parser.stop_sync(&mut ParserHandler {
                        term: &mut view.term,
                        keyboard: &mut view.keyboard,
                        events: &view.events,
                    });
                    view.output_changed(cx);
                }
            });
        });
        self.sync_flush = Some((deadline, task));
    }

    fn output_changed(&mut self, cx: &mut Context<Self>) {
        if !self.mode().contains(TermMode::ALT_SCREEN) {
            if std::mem::take(&mut self.primary_reflowed) {
                self.timeline.resized(self.term.grid());
            }
            let time = Local::now().format("%H:%M:%S").to_string();
            self.timeline.output(self.term.grid(), Arc::from(time));
        }
        self.handle_terminal_events();
        cx.notify();
    }

    pub fn send_draft(&mut self, text: &str, execute: bool, cx: &mut Context<Self>) {
        let mut bytes = input::paste(text, self.mode().contains(TermMode::BRACKETED_PASTE));
        if execute {
            bytes.push(b'\r');
        }
        self.send_input(bytes, cx);
    }

    fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if self.term.grid().display_offset() > 0 || self.term.selection.take().is_some() {
            self.term.scroll_display(Scroll::Bottom);
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
            self.term.resize(size);
            if self.mode().contains(TermMode::ALT_SCREEN) {
                self.primary_reflowed = true;
            } else {
                self.timeline.resized(self.term.grid());
            }
        }
        (self.send)(TerminalCommand::Resize(size));
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

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let m = key.modifiers;
        let clipboard_shortcut = m.platform || (m.control && m.shift && !m.alt);
        let handled = match key.key.as_str() {
            "c" if clipboard_shortcut => {
                if let Some(text) = self.term.selection_to_string() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                true
            }
            "v" if clipboard_shortcut => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.send_draft(&text, false, cx);
                }
                true
            }
            "pageup" | "pagedown" if m.shift && !m.control && !m.alt && !m.platform => {
                self.term.scroll_display(if key.key == "pageup" {
                    Scroll::PageUp
                } else {
                    Scroll::PageDown
                });
                cx.notify();
                true
            }
            _ => {
                let mode = self.mode();
                if let Some(bytes) = input::key_bytes(key, mode | self.keyboard.mode(mode)) {
                    self.send_input(bytes, cx);
                    true
                } else {
                    false
                }
            }
        };
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn mouse_reporting(&self, modifiers: Modifiers) -> bool {
        !modifiers.shift
            && self.mode().intersects(TermMode::MOUSE_MODE)
            && self.term.grid().display_offset() == 0
    }

    fn report_mouse(
        &self,
        button: u8,
        position: (usize, usize),
        release: bool,
        modifiers: Modifiers,
    ) {
        let (column, row) = position;
        if let Some(bytes) =
            input::mouse_bytes(self.mode(), button, column, row, release, modifiers)
        {
            (self.send)(TerminalCommand::Data(bytes));
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window, cx);
        let Some((column, row, side)) = self
            .geometry
            .and_then(|geometry| geometry.locate(event.position, false))
        else {
            return;
        };
        if self.mouse_reporting(event.modifiers) {
            if let Some(button) = input::mouse_button(event.button) {
                self.reported_button = Some(event.button);
                self.last_mouse_cell = Some((column, row));
                self.report_mouse(button, (column, row), false, event.modifiers);
            }
        } else if event.button == MouseButton::Left {
            let ty = match event.click_count {
                2 => SelectionType::Semantic,
                3.. => SelectionType::Lines,
                _ => SelectionType::Simple,
            };
            let point = self.selection_point(column, row);
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
            let point = self.selection_point(column, row);
            if let Some(selection) = &mut self.term.selection {
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
                self.term.scroll_display(Scroll::Delta(count));
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    fn selection_point(&self, column: usize, row: usize) -> CellPoint {
        CellPoint::new(
            Line(row as i32 - self.term.grid().display_offset() as i32),
            Column(column),
        )
    }
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
        let focus = self.focus.clone();
        let surface = canvas(
            move |bounds, window, cx| {
                entity.update(cx, |view, cx| {
                    let text_system = window.text_system();
                    let (font, cell_width) = view
                        .font_metrics
                        .get_or_insert_with(|| {
                            let font = terminal_font(text_system);
                            let font_id = text_system.resolve_font(&font);
                            let width = text_system
                                .ch_advance(font_id, px(13.))
                                .unwrap_or(px(8.))
                                .max(px(1.));
                            (font, width)
                        })
                        .clone();
                    let raw = view.raw();
                    let geometry = Geometry::new(
                        bounds,
                        cell_width,
                        view.show_gutter && !raw,
                        view.compact,
                        raw,
                    );
                    view.apply_geometry(geometry, cx);
                    Frame::new(
                        &view.term,
                        &view.timeline,
                        geometry,
                        Palette::new(view.light),
                        font,
                        view.preedit.clone(),
                    )
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

        let mut viewport = div()
            .id("terminal-surface")
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .bg(rgb(Palette::new(self.light).background))
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
        viewport
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
        if let Some(text) = item.text() {
            self.send_draft(&text, false, cx);
        }
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
        let origin = point(
            element.origin.x + geometry.cell_width * cursor.column.0 as f32,
            element.origin.y + geometry.line_height * cursor.line.0 as f32,
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
