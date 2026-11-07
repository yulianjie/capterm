use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line, Point as CellPoint},
    term::{Term, cell::Flags, color::Colors},
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use gpui_kit::{
    App, Bounds, Font, FontFeatures, FontStyle, FontWeight, Hsla, Pixels, Point, TextAlign,
    TextRun, TextSystem, Window, fill, font, point, px, rgb, size,
};

use super::{Geometry, TerminalEvents, timeline::Timeline};

const ANSI: [u32; 16] = [
    0x0e131c, 0xf07882, 0x86dba8, 0xf2c982, 0x84b6f5, 0xdba3ee, 0x6ce2d0, 0xc0ccda, 0x7c8da3,
    0xffa7ae, 0xa3f1bb, 0xffdc9e, 0xa7cdfd, 0xeec4fa, 0x9ef1e6, 0xe8eef6,
];
const ANSI_LIGHT: [u32; 16] = [
    0x182737, 0xa92b39, 0x236c45, 0x835700, 0x235b9b, 0x893f9b, 0x006e64, 0x536a80, 0x475c71,
    0xc93a47, 0x27834e, 0x956700, 0x2f70b8, 0xa44db9, 0x007e75, 0x182737,
];

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) background: u32,
    foreground: u32,
    cursor: u32,
    panel: u32,
    border: u32,
    dim: u32,
    select: u32,
    ansi: &'static [u32; 16],
}

impl Palette {
    pub(super) fn new(light: bool) -> Self {
        if light {
            Self {
                background: 0xfaf9f7,
                foreground: 0x23232c,
                cursor: 0x4b508b,
                panel: 0xf2f1ef,
                border: 0xd8d6da,
                dim: 0x626575,
                select: 0xe7e7f3,
                ansi: &ANSI_LIGHT,
            }
        } else {
            Self {
                background: 0x111216,
                foreground: 0xececf1,
                cursor: 0xb6bcf2,
                panel: 0x191a1f,
                border: 0x30313a,
                dim: 0x898b9a,
                select: 0x292b40,
                ansi: &ANSI,
            }
        }
    }

    pub(super) fn indexed(self, index: usize, colors: &Colors) -> u32 {
        if index < alacritty_terminal::term::color::COUNT
            && let Some(value) = colors[index]
        {
            return rgb_value(value);
        }
        match index {
            0..16 => self.ansi[index],
            16..232 => {
                let index = (index - 16) as u32;
                let level = |n| if n == 0 { 0 } else { 55 + n * 40 };
                (level(index / 36) << 16) | (level(index / 6 % 6) << 8) | level(index % 6)
            }
            232..=255 => (8 + (index - 232) as u32 * 10) * 0x010101,
            257 => self.background,
            258 => self.cursor,
            259..=266 => dim(self.ansi[index - 259]),
            268 => dim(self.foreground),
            _ => self.foreground,
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

struct Row {
    number: Option<String>,
    stamp: Option<std::sync::Arc<str>>,
    runs: Vec<Run>,
}

pub(super) struct Frame {
    pub(super) geometry: Geometry,
    palette: Palette,
    font: Font,
    rows: Vec<Row>,
    cursor: Option<(CursorShape, usize, usize, bool)>,
    preedit: String,
}

impl Frame {
    pub(super) fn new(
        term: &Term<TerminalEvents>,
        timeline: &Timeline,
        geometry: Geometry,
        palette: Palette,
        font: Font,
        preedit: String,
    ) -> Self {
        let content = term.renderable_content();
        let grid = term.grid();
        let colors = content.colors;
        let top = -(content.display_offset as i32);
        let cursor = (content.display_offset == 0 && content.cursor.shape != CursorShape::Hidden)
            .then(|| {
                let pt = content.cursor.point;
                let shape = if term.is_focused {
                    content.cursor.shape
                } else {
                    CursorShape::HollowBlock
                };
                (
                    shape,
                    pt.line.0 as usize,
                    pt.column.0,
                    grid[pt].flags.contains(Flags::WIDE_CHAR),
                )
            });
        let rows = (0..grid.screen_lines()).map(|row| {
            let line = Line(top + row as i32);
            let stamp = (geometry.gutter_width() > px(0.))
                .then(|| timeline.time(grid, line))
                .flatten();
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
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let selected = content.selection.as_ref().is_some_and(|selection| {
                    selection.contains(pt)
                        || (wide && selection.contains(CellPoint::new(line, Column(column + 1))))
                });
                if selected {
                    fg = palette.foreground;
                    bg = palette.select;
                }
                if matches!(cursor, Some((CursorShape::Block, r, c, _)) if r == row && c == column) {
                    fg = palette.background;
                    bg = palette.indexed(NamedColor::Cursor as usize, colors);
                }
                let flags = cell.flags & (Flags::BOLD | Flags::ITALIC | Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
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
            let number = (geometry.gutter_width() > px(0.)
                && (stamp.is_some() || runs.iter().any(|run| !run.text.trim_matches(' ').is_empty())))
                .then(|| format!("{:03}", grid.history_size() as i32 + line.0 + 1));
            Row { number, stamp, runs }
        }).collect();
        // Use the same default background for empty cells and the viewport.
        let palette = Palette {
            background: palette.indexed(NamedColor::Background as usize, colors),
            ..palette
        };
        Self {
            geometry,
            palette,
            font,
            rows,
            cursor,
            preedit,
        }
    }

    pub(super) fn paint(self, window: &mut Window, cx: &mut App) {
        let geometry = self.geometry;
        let palette = self.palette;
        let grid = geometry.grid;
        let height = geometry.line_height;
        let cell = geometry.cell_width;
        window.paint_quad(fill(geometry.bounds, rgb(palette.background)));

        if let Some(last) = self.rows.iter().rposition(|row| row.number.is_some()) {
            let origin = point(geometry.bounds.origin.x, grid.origin.y);
            let height = height * (last + 1) as f32;
            window.paint_quad(fill(
                Bounds::new(origin, size(geometry.gutter_width(), height)),
                rgb(palette.panel),
            ));
            let border = point(origin.x + geometry.gutter_width() - px(1.), origin.y);
            window.paint_quad(fill(
                Bounds::new(border, size(px(1.), height)),
                rgb(palette.border),
            ));
        }
        for (row, data) in self.rows.into_iter().enumerate() {
            let y = grid.origin.y + height * row as f32;
            if let Some(number) = data.number {
                let padding = if geometry.number_width < px(50.) {
                    px(6.)
                } else {
                    px(12.)
                };
                text(
                    window,
                    cx,
                    point(geometry.bounds.origin.x, y),
                    number,
                    px(11.),
                    height,
                    self.font.clone(),
                    rgb(palette.dim).into(),
                    Some(geometry.number_width - padding),
                    None,
                );
            }
            if let Some(stamp) = data.stamp {
                text(
                    window,
                    cx,
                    point(geometry.bounds.origin.x + geometry.number_width, y),
                    stamp.to_string(),
                    px(11.),
                    height,
                    self.font.clone(),
                    rgb(palette.dim).into(),
                    None,
                    None,
                );
            }
            for run in data.runs {
                let origin = point(grid.origin.x + cell * run.column as f32, y);
                let width = cell * run.columns as f32;
                let style = run.style;
                if style.bg != palette.background {
                    window.paint_quad(fill(
                        Bounds::new(origin, size(width, height)),
                        rgb(style.bg),
                    ));
                }
                if style.flags.intersects(Flags::ALL_UNDERLINES) {
                    let line = point(origin.x, origin.y + height - px(4.));
                    window.paint_quad(fill(
                        Bounds::new(line, size(width, px(1.))),
                        rgb(style.underline),
                    ));
                    if style.flags.contains(Flags::DOUBLE_UNDERLINE) {
                        let second = point(line.x, line.y + px(2.));
                        window.paint_quad(fill(
                            Bounds::new(second, size(width, px(1.))),
                            rgb(style.underline),
                        ));
                    }
                }
                if style.flags.contains(Flags::STRIKEOUT) {
                    let line = point(origin.x, origin.y + height * 0.54);
                    window.paint_quad(fill(Bounds::new(line, size(width, px(1.))), rgb(style.fg)));
                }
                let mut font = self.font.clone();
                if style.flags.contains(Flags::BOLD) {
                    font.weight = FontWeight::BOLD;
                }
                if style.flags.contains(Flags::ITALIC) {
                    font.style = FontStyle::Italic;
                }
                let force_cell_width = run.ascii.then_some(cell);
                text(
                    window,
                    cx,
                    origin,
                    run.text,
                    px(13.),
                    height,
                    font,
                    rgb(style.fg).into(),
                    None,
                    force_cell_width,
                );
            }
        }
        if let Some((shape, row, column, wide)) = self.cursor {
            let origin = point(
                grid.origin.x + cell * column as f32,
                grid.origin.y + height * row as f32,
            );
            let width = cell * if wide { 2. } else { 1. };
            let color = rgb(palette.cursor);
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
                    px(13.),
                    height,
                    self.font,
                    rgb(palette.cursor).into(),
                    None,
                    None,
                );
            }
        }
    }
}

pub(super) fn terminal_font(text_system: &TextSystem) -> Font {
    let family = if cfg!(target_os = "windows") {
        if text_system
            .all_font_names()
            .iter()
            .any(|name| name.eq_ignore_ascii_case("Cascadia Mono"))
        {
            "Cascadia Mono"
        } else {
            "Consolas"
        }
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    };
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
