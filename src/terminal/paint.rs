use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::{Column, Line, Point as CellPoint},
    term::{
        Term, TermMode,
        cell::{Cell, Flags},
        color::Colors,
        search::Match,
    },
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use gpui_kit::{
    App, Bounds, ContentMask, Font, FontFeatures, FontStyle, FontWeight, Hsla, Pixels, Point,
    TextAlign, TextRun, TextSystem, Window, fill, font, point, px, rgb, rgba, size,
};

use super::{
    Geometry, TerminalEvents, TimestampFormat, cell_graphics, format_duration,
    highlight::{Ink, LinuxHighlights},
    prompt::{PromptColors, cell_chars, logical_cells},
    timeline::{SystemLine, Timeline, wraps},
};

#[derive(Clone, Copy, serde::Deserialize)]
struct Hex(#[serde(deserialize_with = "parse_hex")] u32);

fn parse_hex<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    use serde::{Deserialize, de::Error};
    let value = String::deserialize(deserializer)?;
    u32::from_str_radix(value.trim_start_matches('#'), 16).map_err(Error::custom)
}

#[derive(Clone, Copy, serde::Deserialize)]
struct Gutter {
    line_number: Hex,
    timestamp: Hex,
    timestamp_repeated: Hex,
    divider: Hex,
    block_guide: Hex,
    command_ok: Hex,
    command_failed: Hex,
    command_running: Hex,
    live_region: Hex,
    idle_gap: Hex,
}

#[derive(Clone, Copy, serde::Deserialize)]
struct Ruler {
    command: Hex,
    failed: Hex,
    search: Hex,
}

#[derive(Clone, Copy, serde::Deserialize)]
pub(super) struct Palette {
    background: Hex,
    foreground: Hex,
    cursor: Hex,
    cursor_text: Hex,
    selection: Hex,
    search_match: Hex,
    search_match_current: Hex,
    ansi: [Hex; 16],
    gutter: Gutter,
    overview_ruler: Ruler,
    #[serde(skip)]
    light: bool,
}

impl Palette {
    pub(super) fn new(light: bool) -> Self {
        #[derive(serde::Deserialize)]
        struct Themes {
            themes: std::collections::HashMap<String, Palette>,
        }
        static THEMES: std::sync::OnceLock<Themes> = std::sync::OnceLock::new();
        let themes = THEMES.get_or_init(|| {
            serde_json::from_str(include_str!("../../design/theme/terminal-colors.json"))
                .expect("bundled CapTerm terminal palette")
        });
        let mut palette = themes.themes[if light {
            "CapTerm Day"
        } else {
            "CapTerm Night"
        }];
        palette.light = light;
        palette
    }

    pub(super) fn background(self) -> u32 {
        self.background.0
    }
    pub(super) fn raised(self) -> u32 {
        if self.light { 0xffffff } else { 0x1b2129 }
    }
    pub(super) fn text(self) -> u32 {
        if self.light { 0x17202a } else { 0xe4e9ee }
    }
    pub(super) fn secondary(self) -> u32 {
        if self.light { 0x4a5664 } else { 0xa2aebb }
    }
    pub(super) fn border(self) -> u32 {
        if self.light { 0xbcc6d0 } else { 0x313b47 }
    }
    pub(super) fn active(self) -> u32 {
        if self.light { 0xd9eef7 } else { 0x16303b }
    }
    pub(super) fn accent(self) -> u32 {
        self.cursor.0
    }

    pub(super) fn indexed(self, index: usize, colors: &Colors) -> u32 {
        if index < alacritty_terminal::term::color::COUNT
            && let Some(value) = colors[index]
        {
            return rgb_value(value);
        }
        match index {
            0..16 => self.ansi[index].0,
            16..232 => {
                let index = (index - 16) as u32;
                let level = |n| if n == 0 { 0 } else { 55 + n * 40 };
                (level(index / 36) << 16) | (level(index / 6 % 6) << 8) | level(index % 6)
            }
            232..=255 => (8 + (index - 232) as u32 * 10) * 0x010101,
            257 => self.background.0,
            258 => self.cursor.0,
            259..=266 => dim(self.ansi[index - 259].0),
            268 => dim(self.foreground.0),
            _ => self.foreground.0,
        }
    }

    fn resolve(self, value: Color, colors: &Colors, flags: Flags) -> u32 {
        let (value, dimmed) = match value {
            Color::Spec(value) => (rgb_value(value), flags.contains(Flags::DIM)),
            Color::Indexed(index) => (
                self.indexed(index as usize, colors),
                flags.contains(Flags::DIM),
            ),
            Color::Named(named) => {
                let index = if flags.contains(Flags::DIM) {
                    named.to_dim() as usize
                } else {
                    named as usize
                };
                (self.indexed(index, colors), false)
            }
        };
        if dimmed { dim(value) } else { value }
    }
}

fn blend(base: u32, foreground: Hex) -> u32 {
    let alpha = foreground.0 & 255;
    let channel = |shift: u32| {
        let base = (base >> shift) & 255;
        let foreground = (foreground.0 >> (shift + 8)) & 255;
        ((foreground * alpha + base * (255 - alpha) + 127) / 255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

fn dim(value: u32) -> u32 {
    let component = |shift: u32| (((value >> shift) & 0xff) * 2 / 3) << shift;
    component(16) | component(8) | component(0)
}

fn rgb_value(value: Rgb) -> u32 {
    (u32::from(value.r) << 16) | (u32::from(value.g) << 8) | u32::from(value.b)
}

pub(super) fn color_components(value: u32) -> Rgb {
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: u32,
    bg: u32,
    flags: Flags,
    underline: u32,
}

struct Run {
    column: usize,
    columns: usize,
    text: String,
    style: Style,
    ascii: bool,
}

#[derive(Default)]
struct Row {
    number: Option<String>,
    stamp: Option<(String, u32)>,
    decoration: Decoration,
    runs: Vec<Run>,
}

#[derive(Default)]
enum Decoration {
    #[default]
    None,
    System(String),
    Gap(String),
    Live,
    Command {
        collapsed: bool,
        summary: String,
        color: u32,
    },
    Output {
        end: bool,
    },
}

enum DisplayRow<'a> {
    Cell(Line),
    System(&'a SystemLine),
    Gap(String),
}

pub(super) struct FrameOptions<'a> {
    pub(super) time: TimestampFormat,
    pub(super) idle: std::time::Duration,
    pub(super) dim_repeated: bool,
    pub(super) timeline_offset: usize,
    pub(super) show_timeline: bool,
    pub(super) linux_highlights: Option<&'a LinuxHighlights>,
    pub(super) matches: &'a [Match],
    pub(super) current: usize,
}

pub(super) struct Frame {
    pub(super) geometry: Geometry,
    pub(super) line_map: Vec<Option<Line>>,
    pub(super) timeline_overflow: usize,
    palette: Palette,
    font: Font,
    rows: Vec<Row>,
    cursor: Option<(CursorShape, usize, usize, bool)>,
    preedit: String,
    ruler: Option<RulerFrame>,
}

struct RulerFrame {
    top: f32,
    height: f32,
    marks: Vec<(f32, u32, bool)>,
}

struct LineInks {
    first: Line,
    last: Line,
    cells: Vec<(CellPoint, Ink)>,
}

impl LineInks {
    fn new(
        grid: &Grid<Cell>,
        line: Line,
        prompt: Option<&PromptColors>,
        rules: &LinuxHighlights,
    ) -> Option<Self> {
        // Bound the work for a logical line even if it wraps through scrollback.
        const MAX_BYTES: usize = 4096;
        let max_rows = MAX_BYTES / grid.columns().saturating_sub(1).max(1) + 1;
        let mut first = line;
        while first > grid.topmost_line() && wraps(grid, first - 1) {
            first -= 1;
            if (line - first).0 as usize >= max_rows {
                return None;
            }
        }
        let mut last = first;
        let mut text = String::new();
        let mut positions = Vec::new();
        for (point, cell) in logical_cells(grid, first) {
            last = point.line;
            positions.push((point, text.len()));
            text.extend(cell_chars(cell));
            if text.len() > MAX_BYTES {
                return None;
            }
        }
        text.truncate(text.trim_end().len());
        let prompt = prompt.and_then(|prompt| {
            let start = positions.partition_point(|(point, _)| point < prompt.span.start());
            let end = positions.partition_point(|(point, _)| point <= prompt.span.end());
            let start = positions.get(start)?.1;
            let end = positions.get(end).map_or(text.len(), |(_, offset)| *offset);
            (start < end && end <= text.len()).then_some(start..end)
        });
        let inks = rules.colors(&text, prompt);
        let cells = positions
            .into_iter()
            .filter_map(|(point, offset)| Some((point, inks.get(offset).copied().flatten()?)))
            .collect();
        Some(Self { first, last, cells })
    }

    fn on_line(&self, line: Line) -> &[(CellPoint, Ink)] {
        let start = self.cells.partition_point(|(point, _)| point.line < line);
        let end = self.cells.partition_point(|(point, _)| point.line <= line);
        &self.cells[start..end]
    }
}

impl Frame {
    pub(super) fn new(
        term: &Term<TerminalEvents>,
        timeline: &Timeline,
        geometry: Geometry,
        palette: Palette,
        font: Font,
        preedit: String,
        options: FrameOptions<'_>,
    ) -> Self {
        let content = term.renderable_content();
        let grid = term.grid();
        let colors = content.colors;
        let top = Line(-(content.display_offset as i32));
        let alternate = term.mode().contains(TermMode::ALT_SCREEN);
        let virtual_rows = !alternate && options.show_timeline;
        let linux_highlights = options.linux_highlights.filter(|_| virtual_rows);
        let screen_lines = grid.screen_lines();
        let mut display = Vec::with_capacity(screen_lines);
        let mut last_number = None;
        let mut inserted_tail = false;
        for line in (top.0..=grid.bottommost_line().0).map(Line) {
            if virtual_rows {
                if let Some(stamp) = timeline.stamp(grid, line) {
                    let first = last_number != Some(stamp.number);
                    last_number = Some(stamp.number);
                    if let Some((start, command)) = timeline.command_for(stamp.number)
                        && start != stamp.number
                        && command.collapsed
                    {
                        continue;
                    }
                    if first && let Some(messages) = timeline.systems.get(&stamp.number) {
                        display.extend(messages.iter().map(|message| DisplayRow::System(message)));
                    }
                    if first
                        && line > grid.topmost_line()
                        && let Some(previous) = timeline.stamp(grid, line - 1)
                        && let Ok(idle) = (stamp.time - previous.time).to_std()
                        && idle > options.idle
                    {
                        display.push(DisplayRow::Gap(format!(
                            "◷  空闲 {}",
                            format_duration(idle)
                        )));
                    }
                } else if !inserted_tail && line >= grid.cursor.point.line {
                    inserted_tail = true;
                    display.extend(timeline.pending_systems().iter().map(DisplayRow::System));
                }
            }
            display.push(DisplayRow::Cell(line));
            if content.display_offset > 0 && display.len() >= screen_lines {
                break;
            }
        }
        let mut timeline_overflow = 0;
        if content.display_offset == 0 {
            let active = display.iter().position(
                |row| matches!(row, DisplayRow::Cell(line) if *line == content.cursor.point.line),
            );
            let following =
                if virtual_rows && !inserted_tail && !timeline.pending_systems().is_empty() {
                    display.extend(timeline.pending_systems().iter().map(DisplayRow::System));
                    display.len()
                } else {
                    active.map_or(0, |row| row + 1)
                };
            timeline_overflow = following.saturating_sub(screen_lines);
            display.drain(..timeline_overflow.saturating_sub(options.timeline_offset));
        }
        display.truncate(screen_lines);
        let line_map: Vec<_> = display
            .iter()
            .map(|row| match row {
                DisplayRow::Cell(line) => Some(*line),
                _ => None,
            })
            .collect();
        let cursor = (content.display_offset == 0 && content.cursor.shape != CursorShape::Hidden)
            .then(|| {
                let pt = content.cursor.point;
                let row = line_map.iter().position(|line| *line == Some(pt.line))?;
                let shape = if term.is_focused {
                    content.cursor.shape
                } else {
                    CursorShape::HollowBlock
                };
                Some((
                    shape,
                    row,
                    pt.column.0,
                    grid[pt].flags.contains(Flags::WIDE_CHAR),
                ))
            })
            .flatten();
        let mut painted_prompt = None;
        let mut line_inks: Option<LineInks> = None;
        let rows = display.into_iter().enumerate().map(|(row, display_row)| {
            let line = match display_row {
                DisplayRow::Cell(line) => line,
                DisplayRow::System(message) => return Row {
                    decoration: Decoration::System(message.text.clone()),
                    stamp: (geometry.time_width > px(0.)).then(|| (timeline.timestamp(message.time, options.time), palette.gutter.timestamp.0)),
                    ..Row::default()
                },
                DisplayRow::Gap(gap) => return Row { decoration: Decoration::Gap(gap), ..Row::default() },
            };
            let metadata = virtual_rows.then(|| timeline.stamp(grid, line)).flatten();
            if let Some(stamp) = metadata
                && painted_prompt.as_ref().is_none_or(|(number, _)| *number != stamp.number)
            {
                painted_prompt = Some((
                    stamp.number,
                    timeline.prompt_colors(grid, line, stamp.number),
                ));
            }
            let prompt = metadata
                .and(painted_prompt.as_ref())
                .and_then(|(_, colors)| colors.as_ref());
            if let Some(rules) = linux_highlights
                && line_inks.as_ref().is_none_or(|inks| line < inks.first || line > inks.last)
            {
                line_inks = LineInks::new(grid, line, prompt, rules);
            }
            let mut inks = line_inks
                .as_ref()
                .map_or(&[][..], |inks| inks.on_line(line))
                .iter()
                .peekable();
            let continued = !alternate && timeline.continued_from(grid, line).is_some();
            let previous = (line > grid.topmost_line()).then(|| timeline.stamp(grid, line - 1)).flatten();
            let number = if geometry.number_width > px(0.) {
                if alternate {
                    Some((row + 1).to_string())
                } else {
                    metadata.filter(|_| !continued).map(|stamp| stamp.number.to_string())
                }
            } else { None };
            let live = metadata.is_some_and(|stamp| stamp.live && line.0 >= 0);
            let stamp = metadata.filter(|_| geometry.time_width > px(0.) && (!continued || live)).map(|stamp| {
                if live {
                    ("实时".to_owned(), palette.accent())
                } else {
                    let repeated = options.dim_repeated && previous.is_some_and(|previous| previous.time.timestamp() == stamp.time.timestamp());
                    let color = if repeated { palette.gutter.timestamp_repeated } else { palette.gutter.timestamp };
                    (timeline.timestamp(stamp.time, options.time), color.0)
                }
            });
            let command = metadata.and_then(|stamp| timeline.command_for(stamp.number).map(|(start, command)| (stamp.number, start, command)));
            let decoration = if live {
                Decoration::Live
            } else if let Some((number, start, command)) = command {
                if start == number && !continued {
                    let (summary, color) = match command.status {
                        Some(0) => (format!("✓ {}", command.elapsed.map(format_duration).unwrap_or_default()), palette.gutter.command_ok),
                        Some(code) => (format!("✕ 退出码 {code}  {}", command.elapsed.map(format_duration).unwrap_or_default()), palette.gutter.command_failed),
                        None if command.running() => ("运行中".to_owned(), palette.gutter.command_running),
                        None if command.elapsed.is_some() => (command.elapsed.map(format_duration).unwrap_or_default(), palette.gutter.timestamp),
                        None => (String::new(), palette.gutter.timestamp),
                    };
                    Decoration::Command { collapsed: command.collapsed, summary, color: color.0 }
                } else if start != number {
                    Decoration::Output { end: command.last_line == Some(number) && !super::timeline::wraps(grid, line) }
                } else { Decoration::None }
            } else { Decoration::None };
            let mut hit_index = options.matches.partition_point(|hit| hit.end().line < line);
            let mut runs: Vec<Run> = Vec::new();
            for column in 0..grid.columns() {
                let pt = CellPoint::new(line, Column(column));
                let cell = &grid[pt];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let wide = cell.flags.contains(Flags::WIDE_CHAR);
                let columns = if wide { 2 } else { 1 };
                let mut fg = palette.resolve(cell.fg, colors, cell.flags);
                let mut bg = palette.resolve(cell.bg, colors, Flags::empty());
                let ink = inks.next_if(|(point, _)| *point == pt).map(|(_, ink)| *ink);
                if cell.fg == Color::Named(NamedColor::Foreground)
                    && !cell.flags.intersects(Flags::INVERSE | Flags::DIM)
                {
                    if let Some(color) = prompt.and_then(|prompt| prompt.foreground(pt)) {
                        fg = palette.ansi[color as usize].0;
                    } else if let Some(ink) = ink {
                        fg = match ink {
                            Ink::Ansi(index) => palette.indexed(index, colors),
                            Ink::Rgb(value) => value,
                        };
                    }
                }
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let selected = content.selection.as_ref().is_some_and(|selection| {
                    selection.contains(pt)
                        || (wide && selection.contains(CellPoint::new(line, Column(column + 1))))
                });
                while options.matches.get(hit_index).is_some_and(|hit| *hit.end() < pt) {
                    hit_index += 1;
                }
                if let Some(hit) = options.matches.get(hit_index)
                    && (hit.contains(&pt) || (wide && hit.contains(&CellPoint::new(line, Column(column + 1)))))
                {
                    bg = blend(bg, if hit_index == options.current { palette.search_match_current } else { palette.search_match });
                }
                if selected {
                    bg = blend(bg, palette.selection);
                }
                if matches!(cursor, Some((CursorShape::Block, r, c, _)) if r == row && c == column) {
                    fg = palette.cursor_text.0;
                    bg = palette.indexed(NamedColor::Cursor as usize, colors);
                }
                let mut flags = cell.flags & (Flags::BOLD | Flags::ITALIC | Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
                if cell.hyperlink().is_some() && !flags.intersects(Flags::ALL_UNDERLINES) {
                    flags.insert(Flags::UNDERLINE);
                }
                let underline = cell.underline_color().map_or(fg, |color| palette.resolve(color, colors, cell.flags));
                let style = Style { fg, bg, flags, underline };
                let hidden = cell.flags.intersects(Flags::HIDDEN | Flags::LEADING_WIDE_CHAR_SPACER);
                let text = if hidden { ' ' } else { cell.c };
                let combining = cell.zerowidth().filter(|_| !hidden).unwrap_or_default();
                if text == ' ' && combining.is_empty() && flags.is_empty()
                    && bg == palette.indexed(NamedColor::Background as usize, colors)
                {
                    continue;
                }
                let ascii = text.is_ascii() && combining.is_empty() && !wide;
                if let Some(last) = runs.last_mut().filter(|last| {
                    ascii && last.ascii && last.style == style && last.column + last.columns == column
                }) {
                    last.text.push(text);
                    last.columns += 1;
                } else {
                    let text = std::iter::once(text).chain(combining.iter().copied()).collect();
                    runs.push(Run { column, columns, text, style, ascii });
                }
            }
            Row { number, stamp, decoration, runs }
        }).collect();
        let ruler =
            (!alternate).then(|| {
                let total = (grid.total_lines() + timeline_overflow).max(1) as f32;
                let oldest = grid.topmost_line();
                let mut marks = Vec::new();
                let first = (oldest.0..=grid.bottommost_line().0)
                    .find_map(|line| timeline.stamp(grid, Line(line)))
                    .map(|stamp| stamp.number);
                let last = (oldest.0..=grid.bottommost_line().0)
                    .rev()
                    .find_map(|line| timeline.stamp(grid, Line(line)))
                    .map(|stamp| stamp.number);
                if let (Some(first), Some(last)) = (first, last)
                    && first <= last
                {
                    let span = (last.saturating_sub(first) + 1) as f32;
                    marks.extend(timeline.commands.range(first..=last).map(
                        |(&number, command)| {
                            let color = if command.status.is_some_and(|code| code != 0) {
                                palette.overview_ruler.failed.0
                            } else {
                                blend(palette.background.0, palette.overview_ruler.command)
                            };
                            ((number - first) as f32 / span, color, false)
                        },
                    ));
                }
                marks.extend(options.matches.iter().map(|hit| {
                    (
                        (hit.start().line.0 - oldest.0) as f32 / total,
                        palette.overview_ruler.search.0,
                        true,
                    )
                }));
                RulerFrame {
                    top: ((top.0 - oldest.0) as usize
                        + timeline_overflow.saturating_sub(options.timeline_offset))
                        as f32
                        / total,
                    height: screen_lines as f32 / total,
                    marks,
                }
            });
        let palette = Palette {
            background: Hex(palette.indexed(NamedColor::Background as usize, colors)),
            cursor: Hex(palette.indexed(NamedColor::Cursor as usize, colors)),
            ..palette
        };
        Self {
            geometry,
            line_map,
            timeline_overflow,
            palette,
            font,
            rows,
            cursor,
            preedit,
            ruler,
        }
    }

    pub(super) fn paint(self, window: &mut Window, cx: &mut App) {
        let geometry = self.geometry;
        let palette = self.palette;
        let grid = geometry.grid;
        let height = geometry.line_height;
        let cell = geometry.cell_width;
        window.paint_quad(fill(geometry.bounds, rgb(palette.background.0)));

        if geometry.gutter_width() > px(0.) {
            let divider = point(
                geometry.bounds.origin.x + geometry.gutter_width() - px(1.),
                grid.origin.y,
            );
            window.paint_quad(fill(
                Bounds::new(divider, size(px(1.), geometry.bounds.size.height)),
                rgb(palette.gutter.divider.0),
            ));
        }
        let interface = font("Noto Sans SC");
        for (row, data) in self.rows.into_iter().enumerate() {
            let y = grid.origin.y + height * row as f32;
            if let Some(number) = data.number {
                let padding = if number.len() > 5 { px(3.) } else { px(10.) };
                text(
                    window,
                    cx,
                    point(geometry.bounds.origin.x, y),
                    number,
                    px(11.5),
                    height,
                    self.font.clone(),
                    rgb(palette.gutter.line_number.0).into(),
                    Some(geometry.number_width - padding),
                    None,
                );
            }
            if let Some((stamp, color)) = data.stamp {
                let font = if stamp == "实时" {
                    interface.clone()
                } else {
                    self.font.clone()
                };
                text(
                    window,
                    cx,
                    point(geometry.bounds.origin.x + geometry.number_width, y),
                    stamp,
                    px(11.5),
                    height,
                    font,
                    rgb(color).into(),
                    None,
                    None,
                );
            }
            let marker = point(grid.origin.x - geometry.marker_width / 2., y);
            let mut summary = None;
            match data.decoration {
                Decoration::System(message) => {
                    let x = if geometry.number_width > px(0.) {
                        geometry.bounds.origin.x + geometry.number_width - px(20.)
                    } else {
                        marker.x - px(3.)
                    };
                    text(
                        window,
                        cx,
                        point(x, y),
                        "◇".into(),
                        px(12.),
                        height,
                        interface.clone(),
                        rgb(palette.secondary()).into(),
                        None,
                        None,
                    );
                    text(
                        window,
                        cx,
                        point(grid.origin.x, y),
                        message,
                        px(12.),
                        height,
                        interface.clone(),
                        rgb(palette.secondary()).into(),
                        None,
                        None,
                    );
                }
                Decoration::Gap(gap) => {
                    let x = grid.origin.x;
                    text(
                        window,
                        cx,
                        point(x, y),
                        gap.clone(),
                        px(11.),
                        height,
                        interface.clone(),
                        rgb(palette.gutter.idle_gap.0).into(),
                        None,
                        None,
                    );
                    let x = x + px(7.) * gap.chars().count() as f32 + px(12.);
                    let right = grid.origin.x + grid.size.width;
                    if x < right {
                        window.paint_quad(fill(
                            Bounds::new(point(x, y + height / 2.), size(right - x, px(1.))),
                            rgb(palette.gutter.divider.0),
                        ));
                    }
                }
                Decoration::Live => window.paint_quad(fill(
                    Bounds::new(
                        point(marker.x - px(1.), y + px(1.)),
                        size(px(3.), height - px(2.)),
                    ),
                    rgba(palette.gutter.live_region.0),
                )),
                Decoration::Command {
                    collapsed,
                    summary: label,
                    color,
                } => {
                    text(
                        window,
                        cx,
                        point(marker.x - px(4.), y),
                        if collapsed {
                            "▸".into()
                        } else {
                            "▾".into()
                        },
                        px(12.),
                        height,
                        interface.clone(),
                        rgb(color).into(),
                        None,
                        None,
                    );
                    summary = (!label.is_empty()).then_some((label, color));
                }
                Decoration::Output { end } => {
                    let h = if end { height - px(6.) } else { height };
                    let color = rgb(palette.gutter.block_guide.0);
                    window.paint_quad(fill(Bounds::new(marker, size(px(1.), h)), color));
                    if end {
                        window.paint_quad(fill(
                            Bounds::new(point(marker.x, y + h), size(px(5.), px(1.))),
                            color,
                        ));
                    }
                }
                Decoration::None => {}
            }
            for run in data.runs {
                let origin = point(grid.origin.x + cell * run.column as f32, y);
                let width = cell * run.columns as f32;
                let bounds = Bounds::new(origin, size(width, height));
                let style = run.style;
                let foreground = rgb(style.fg);
                if style.bg != palette.background.0 {
                    window.paint_quad(fill(bounds, rgb(style.bg)));
                }
                let graphic = !run.ascii
                    && run.columns == 1
                    && cell_graphics::paint(window, &run.text, bounds, foreground.into());
                if style.flags.intersects(Flags::ALL_UNDERLINES) {
                    underline(window, bounds, style);
                }
                if style.flags.contains(Flags::STRIKEOUT) {
                    let line = point(origin.x, origin.y + height * 0.54);
                    window.paint_quad(fill(Bounds::new(line, size(width, px(1.))), foreground));
                }
                if graphic {
                    continue;
                }
                let mut font = self.font.clone();
                if style.flags.contains(Flags::BOLD) {
                    font.weight = FontWeight::BOLD;
                }
                if style.flags.contains(Flags::ITALIC) {
                    font.style = FontStyle::Italic;
                }
                let force_cell_width = run.ascii.then_some(cell);
                let paint_run = |window: &mut Window| {
                    text(
                        window,
                        cx,
                        origin,
                        run.text,
                        geometry.font_size,
                        height,
                        font,
                        foreground.into(),
                        None,
                        force_cell_width,
                    );
                };
                if run.ascii {
                    paint_run(window);
                } else {
                    window.with_content_mask(Some(ContentMask { bounds }), paint_run);
                }
            }
            if let Some((label, color)) = summary {
                text(
                    window,
                    cx,
                    point(grid.origin.x, y),
                    label,
                    px(11.5),
                    height,
                    interface.clone(),
                    rgb(color).into(),
                    Some(grid.size.width - px(4.)),
                    None,
                );
            }
        }
        if let Some(ruler) = self.ruler {
            let right = geometry.bounds.origin.x + geometry.bounds.size.width;
            let x = right - px(12.);
            let y = geometry.bounds.origin.y;
            let ruler_height = geometry.bounds.size.height;
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(px(1.), ruler_height)),
                rgb(palette.gutter.divider.0),
            ));
            if ruler.height < 1. {
                let thumb = (ruler_height * ruler.height).max(px(8.)).min(ruler_height);
                let thumb_y = (y + ruler_height * ruler.top).min(y + ruler_height - thumb);
                window.paint_quad(fill(
                    Bounds::new(point(x + px(2.), thumb_y), size(px(8.), thumb)),
                    rgb(palette.gutter.block_guide.0),
                ));
            }
            let mut last = None;
            for (position, color, search) in ruler.marks {
                let y = y + (ruler_height * position).floor();
                if last == Some((y, color)) {
                    continue;
                }
                last = Some((y, color));
                let start = if search { px(6.) } else { px(3.) };
                window.paint_quad(fill(
                    Bounds::new(point(x + start, y), size(px(10.) - start, px(2.))),
                    rgb(color),
                ));
            }
        }
        if let Some((shape, row, column, wide)) = self.cursor {
            let origin = point(
                grid.origin.x + cell * column as f32,
                grid.origin.y + height * row as f32,
            );
            let width = cell * if wide { 2. } else { 1. };
            let color = rgb(palette.cursor.0);
            let mut draw = |offset_x, offset_y, w, h| {
                window.paint_quad(fill(
                    Bounds::new(point(origin.x + offset_x, origin.y + offset_y), size(w, h)),
                    color,
                ));
            };
            match shape {
                CursorShape::Beam => draw(px(0.), px(2.), px(2.), height - px(4.)),
                CursorShape::Underline => draw(px(0.), height - px(3.), width, px(2.)),
                CursorShape::HollowBlock => {
                    draw(px(0.), px(2.), width, px(1.));
                    draw(px(0.), height - px(3.), width, px(1.));
                    draw(px(0.), px(2.), px(1.), height - px(4.));
                    draw(width - px(1.), px(2.), px(1.), height - px(4.));
                }
                CursorShape::Block | CursorShape::Hidden => {}
            }
            if !self.preedit.is_empty() {
                let preedit = self.preedit.replace(['\n', '\r'], " ");
                text(
                    window,
                    cx,
                    origin,
                    preedit,
                    geometry.font_size,
                    height,
                    self.font,
                    rgb(palette.cursor.0).into(),
                    None,
                    None,
                );
            }
        }
    }
}

fn underline(window: &mut Window, bounds: Bounds<Pixels>, style: Style) {
    let line = point(bounds.left(), bounds.bottom() - px(4.));
    let width = bounds.size.width;
    let color = rgb(style.underline);
    let mut draw = |offset, vertical, length| {
        window.paint_quad(fill(
            Bounds::new(
                point(line.x + offset, line.y + vertical),
                size(length, px(1.)),
            ),
            color,
        ));
    };
    if style.flags.contains(Flags::UNDERCURL) {
        for i in 0..f32::from(width).ceil() as usize {
            let offset = px(i as f32);
            let dy = [0., 0., 1., 2., 2., 1.][i % 6];
            draw(offset, px(dy), (width - offset).min(px(1.)));
        }
    } else if style
        .flags
        .intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE)
    {
        let (step, dash) = if style.flags.contains(Flags::DOTTED_UNDERLINE) {
            (3, 1.)
        } else {
            (6, 4.)
        };
        for i in (0..f32::from(width).ceil() as usize).step_by(step) {
            let offset = px(i as f32);
            draw(offset, px(0.), (width - offset).min(px(dash)));
        }
    } else {
        draw(px(0.), px(0.), width);
        if style.flags.contains(Flags::DOUBLE_UNDERLINE) {
            draw(px(0.), px(2.), width);
        }
    }
}

pub(super) fn terminal_font(text_system: &TextSystem, preferred: Option<&str>) -> Font {
    let family = preferred.unwrap_or_else(|| {
        let available = text_system.all_font_names();
        [
            "Maple Mono NF CN",
            "Maple Mono",
            "Sarasa Mono SC",
            "Cascadia Mono",
        ]
        .into_iter()
        .find(|family| {
            available
                .iter()
                .any(|name| name.eq_ignore_ascii_case(family))
        })
        .unwrap_or(if cfg!(target_os = "windows") {
            "Consolas"
        } else if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "DejaVu Sans Mono"
        })
    });
    let mut font = font(family);
    font.features = FontFeatures::disable_ligatures();
    font
}

#[allow(clippy::too_many_arguments)]
fn text(
    window: &mut Window,
    cx: &mut App,
    origin: Point<Pixels>,
    text: String,
    font_size: Pixels,
    height: Pixels,
    font: Font,
    color: Hsla,
    align_width: Option<Pixels>,
    force_cell_width: Option<Pixels>,
) {
    if text.is_empty() || text.bytes().all(|byte| byte == b' ') {
        return;
    }
    let run = TextRun {
        len: text.len(),
        font,
        color,
        ..TextRun::default()
    };
    let align = if align_width.is_some() {
        TextAlign::Right
    } else {
        TextAlign::Left
    };
    let shaped = window
        .text_system()
        .shape_line(text.into(), font_size, &[run], force_cell_width);
    let _ = shaped.paint(origin, height, align, align_width, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::{term::Config, vte::ansi::Processor};
    use chrono::{Duration, Local};

    fn geometry(alternate: bool) -> Geometry {
        geometry_at_width(alternate, 400.)
    }

    fn geometry_at_width(alternate: bool, width: f32) -> Geometry {
        Geometry::new(
            Bounds::new(point(px(0.), px(0.)), size(px(width), px(64.))),
            px(7.8),
            true,
            true,
            TimestampFormat::Seconds,
            false,
            alternate,
            px(13.),
            px(20.),
        )
    }

    fn frame(
        term: &Term<TerminalEvents>,
        timeline: &Timeline,
        geometry: Geometry,
        offset: usize,
    ) -> Frame {
        frame_with_rules(term, timeline, geometry, offset, None)
    }

    fn frame_with_rules(
        term: &Term<TerminalEvents>,
        timeline: &Timeline,
        geometry: Geometry,
        offset: usize,
        linux_highlights: Option<&LinuxHighlights>,
    ) -> Frame {
        Frame::new(
            term,
            timeline,
            geometry,
            Palette::new(false),
            font("monospace"),
            String::new(),
            FrameOptions {
                time: TimestampFormat::Seconds,
                idle: std::time::Duration::from_secs(60),
                dim_repeated: true,
                timeline_offset: offset,
                show_timeline: true,
                linux_highlights,
                matches: &[],
                current: 0,
            },
        )
    }

    #[test]
    fn inserted_gap_leaves_pty_coordinates_intact_and_older_rows_reachable() {
        let geometry = geometry(false);
        let mut term = Term::new(Config::default(), &geometry.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        let now = Local::now();
        parser.advance(&mut term, b"before");
        timeline.output(term.grid(), now);
        parser.advance(&mut term, b"\r\nafter\r\nlast");
        timeline.output(term.grid(), now + Duration::seconds(61));
        let latest = frame(&term, &timeline, geometry, 0);
        assert_eq!(latest.timeline_overflow, 1);
        assert_eq!(latest.line_map, [None, Some(Line(1)), Some(Line(2))]);
        assert_eq!(
            frame(&term, &timeline, geometry, 1).line_map,
            [Some(Line(0)), None, Some(Line(1))]
        );
        assert_eq!(term.grid().history_size(), 0);
    }

    #[test]
    fn alternate_screen_shows_screen_numbers_without_timestamps_or_ruler() {
        let geometry = geometry(true);
        let mut term = Term::new(Config::default(), &geometry.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?1049hfull screen");
        let frame = frame(&term, &Timeline::default(), geometry, 0);
        assert!(frame.ruler.is_none());
        for (row, number) in frame.rows.iter().zip(["1", "2", "3"]) {
            assert_eq!(row.number.as_deref(), Some(number));
            assert!(row.stamp.is_none());
        }
    }

    fn foreground_at(frame: &Frame, line: Line, column: usize) -> u32 {
        let row = frame
            .line_map
            .iter()
            .position(|&row| row == Some(line))
            .unwrap();
        frame.rows[row]
            .runs
            .iter()
            .find(|run| (run.column..run.column + run.columns).contains(&column))
            .unwrap()
            .style
            .fg
    }

    #[test]
    fn marked_prompt_colors_only_default_foreground_inside_its_boundaries() {
        let geometry = geometry(false);
        let mut term = Term::new(Config::default(), &geometry.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        let now = Local::now();
        parser.advance(&mut term, b"note ");
        timeline.prompt(term.grid(), now);
        parser.advance(
            &mut term,
            b"\x1b[31mde\x1b[0mploy@prod:/srv/\x1b[38;2;17;123;231mapi\x1b[0m$ ",
        );
        timeline.end_prompt(term.grid());
        parser.advance(&mut term, b"git\r\n");
        timeline.start_command(now);
        parser.advance(
            &mut term,
            b"deploy@prod:/srv/api$ \r\n\x1b[35mcolor output\x1b[0m",
        );
        timeline.output(term.grid(), now);

        let frame = frame(&term, &timeline, geometry, 0);
        let fg = |line, column| foreground_at(&frame, Line(line), column);
        assert_eq!(fg(0, 0), 0xd5dce3); // text before OSC 133 A
        assert_eq!(fg(0, 5), 0xf0626a); // explicit red in the user name
        assert_eq!(fg(0, 7), 0x4cc38a); // default user and host
        assert_eq!(fg(0, 16), 0xd5dce3); // separator
        assert_eq!(fg(0, 17), 0x5b9cf0); // default path
        assert_eq!(fg(0, 22), 0x117be7); // explicit RGB in the path
        assert_eq!(fg(0, 27), 0xd5dce3); // command after OSC 133 B
        assert_eq!(fg(1, 0), 0xd5dce3); // prompt-shaped command output
        assert_eq!(fg(2, 0), 0xc58af0); // explicit color in command output
    }

    #[test]
    fn fallback_prompt_colors_survive_reflow_but_not_a_row_rewrite() {
        let narrow = geometry_at_width(false, 280.);
        let mut term = Term::new(Config::default(), &narrow.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        let now = Local::now();
        parser.advance(&mut term, "deploy@prod:/some/very/long/目录$ ".as_bytes());
        timeline.output(term.grid(), now);
        assert!(term.grid().cursor.point.line > Line(0));
        let before = frame(&term, &timeline, narrow, 0);
        assert!(
            before
                .rows
                .iter()
                .flat_map(|row| &row.runs)
                .any(|run| run.text == "目" && run.style.fg == 0x5b9cf0)
        );

        let wide = geometry_at_width(false, 600.);
        let logical = timeline.before_resize(term.grid());
        term.resize(wide.size);
        timeline.resized(term.grid(), logical);
        let reflowed = frame(&term, &timeline, wide, 0);
        assert_eq!(foreground_at(&reflowed, Line(0), 0), 0x4cc38a);
        assert_eq!(foreground_at(&reflowed, Line(0), 12), 0x5b9cf0);

        parser.advance(&mut term, b"\r\x1b[2Kplain output");
        timeline.output(term.grid(), now);
        assert_eq!(
            foreground_at(&frame(&term, &timeline, wide, 0), Line(0), 0),
            0xd5dce3
        );
    }

    #[test]
    fn local_windows_prompts_color_the_path_only() {
        let geometry = geometry(false);
        let mut term = Term::new(Config::default(), &geometry.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        let now = Local::now();
        parser.advance(&mut term, b"PS C:\\work bench> ");
        timeline.output(term.grid(), now);
        parser.advance(&mut term, b"\r\nD:\\project>");
        timeline.output(term.grid(), now);

        let frame = frame(&term, &timeline, geometry, 0);
        assert_eq!(foreground_at(&frame, Line(0), 0), 0xd5dce3); // PS label
        assert_eq!(foreground_at(&frame, Line(0), 3), 0x5b9cf0);
        assert_eq!(foreground_at(&frame, Line(0), 16), 0xd5dce3); // >
        assert_eq!(foreground_at(&frame, Line(1), 0), 0x5b9cf0);
    }

    #[test]
    fn conda_prompt_keeps_its_colors_after_echo_and_colored_output() {
        let geometry = geometry_at_width(false, 950.);
        let mut term = Term::new(Config::default(), &geometry.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        let now = Local::now();
        let prompt = "(py312) developer@0123456789abcdef0123456789abcdef:~$ ";
        parser.advance(&mut term, prompt.as_bytes());
        timeline.output(term.grid(), now);
        timeline.start_fallback(now);
        parser.advance(&mut term, b"ll\r\n\x1b[34magent/\x1b[0m");
        timeline.output(term.grid(), now);

        let frame = frame(&term, &timeline, geometry, 0);
        let fg = |column| foreground_at(&frame, Line(0), column);
        assert_eq!(fg(0), 0xd5dce3); // environment prefix
        assert_eq!(fg("(py312) ".len()), 0x4cc38a); // user
        assert_eq!(fg(prompt.find('@').unwrap() + 1), 0x4cc38a); // host
        assert_eq!(fg(prompt.find('~').unwrap()), 0x5b9cf0);
        assert_eq!(fg(prompt.len()), 0xd5dce3); // echoed command
        assert_eq!(foreground_at(&frame, Line(1), 0), 0x5b9cf0); // ANSI output
    }

    #[test]
    fn linux_rules_color_commands_and_ls_fields_but_preserve_ansi() {
        let geometry = geometry_at_width(false, 950.);
        let mut term = Term::new(Config::default(), &geometry.size, TerminalEvents::default());
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        let now = Local::now();
        let prompt = "(py312) developer@container:~$ ";
        parser.advance(&mut term, prompt.as_bytes());
        timeline.output(term.grid(), now);
        timeline.start_fallback(now);
        let output = "drwxr-xr-x 4 developer developer 4096 Nov 7 05:41 ";
        parser.advance(&mut term, b"ll --all\r\n");
        parser.advance(&mut term, output.as_bytes());
        parser.advance(&mut term, b"\x1b[35magent/\x1b[0m");
        timeline.output(term.grid(), now);

        let rules = LinuxHighlights::bundled();
        let frame = frame_with_rules(&term, &timeline, geometry, 0, Some(&rules));
        let fg = |line, column| foreground_at(&frame, Line(line), column);
        assert_eq!(fg(0, prompt.find("developer").unwrap()), 0x4cc38a);
        assert_eq!(fg(0, prompt.find('~').unwrap()), 0x5b9cf0);
        assert_ne!(fg(0, prompt.find('$').unwrap()), 0xd5dce3);
        assert_ne!(fg(0, prompt.len()), 0xd5dce3); // ll
        assert_ne!(fg(0, prompt.len() + 3), 0xd5dce3); // --all
        assert_ne!(fg(1, 1), fg(1, 2)); // read and write permissions
        assert_ne!(fg(1, output.find("4096").unwrap()), 0xd5dce3);
        assert_ne!(fg(1, output.find("Nov").unwrap()), 0xd5dce3);
        assert_eq!(fg(1, output.len()), 0xc58af0); // program-set foreground

        parser.advance(&mut term, b"\x1b[?1049h\x1b[HERROR 4096");
        let alternate = geometry_at_width(true, 950.);
        let frame = frame_with_rules(&term, &timeline, alternate, 0, Some(&rules));
        assert_eq!(foreground_at(&frame, Line(0), 0), 0xd5dce3);
    }
}
