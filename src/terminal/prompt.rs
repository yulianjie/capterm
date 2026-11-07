use std::ops::{Range, RangeInclusive};

use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::{Column, Line, Point},
    term::cell::{Cell, Flags},
    vte::ansi::NamedColor,
};

use super::timeline::wraps;

const MAX_PROMPT_BYTES: usize = 1024;

pub(super) struct Prompt {
    prefix: String,
    start: usize,
    identity: Option<Range<usize>>,
    path: Range<usize>,
}

pub(super) struct PromptColors {
    pub(super) span: RangeInclusive<Point>,
    identity: Option<RangeInclusive<Point>>,
    path: RangeInclusive<Point>,
}

impl PromptColors {
    pub(super) fn foreground(&self, point: Point) -> Option<NamedColor> {
        if self
            .identity
            .as_ref()
            .is_some_and(|range| range.contains(&point))
        {
            Some(NamedColor::Green)
        } else if self.path.contains(&point) {
            Some(NamedColor::Blue)
        } else {
            None
        }
    }
}

impl Prompt {
    pub(super) fn text(&self) -> &str {
        &self.prefix[self.start..]
    }

    pub(super) fn capture(grid: &Grid<Cell>, first: Line, start_column: usize) -> Option<Self> {
        let cursor = grid.cursor.point;
        let mut prefix = String::new();
        let mut start = None;
        for (point, cell) in logical_cells(grid, first) {
            if point > cursor || (point == cursor && !grid.cursor.input_needs_wrap) {
                break;
            }
            if point.line > first || point.column.0 >= start_column {
                start.get_or_insert(prefix.len());
            }
            prefix.extend(cell_chars(cell));
            if prefix.len() > MAX_PROMPT_BYTES {
                return None;
            }
        }
        let start = start?;
        prefix.truncate(prefix.trim_end().len());
        let (identity, path) = spans(prefix.get(start..)?)?;
        let shift = |range: Range<usize>| range.start + start..range.end + start;
        Some(Self {
            prefix,
            start,
            identity: identity.map(shift),
            path: shift(path),
        })
    }

    pub(super) fn colors(&self, grid: &Grid<Cell>, first: Line) -> Option<PromptColors> {
        // Keep offsets in the captured text so they survive a change in column count.
        // Match the whole prefix before using the ranges in case the shell rewrote the row.
        let mut remaining = self.prefix.as_str();
        let mut first_point = None;
        let mut identity = None;
        let mut path = None;
        for (point, cell) in logical_cells(grid, first) {
            let offset = self.prefix.len() - remaining.len();
            if offset == self.start {
                first_point = Some(point);
            }
            for ch in cell_chars(cell) {
                remaining = remaining.strip_prefix(ch)?;
            }
            if self
                .identity
                .as_ref()
                .is_some_and(|range| range.contains(&offset))
            {
                extend_range(&mut identity, point);
            }
            if self.path.contains(&offset) {
                extend_range(&mut path, point);
            }
            if remaining.is_empty() {
                return Some(PromptColors {
                    span: first_point?..=point,
                    identity,
                    path: path?,
                });
            }
        }
        None
    }
}

fn extend_range(range: &mut Option<RangeInclusive<Point>>, point: Point) {
    *range = Some(range.as_ref().map_or(point, |r| *r.start())..=point);
}

pub(super) fn logical_cells(
    grid: &Grid<Cell>,
    first: Line,
) -> impl Iterator<Item = (Point, &Cell)> {
    std::iter::successors(Some(first), |&line| {
        (line < grid.bottommost_line() && wraps(grid, line)).then_some(line + 1)
    })
    .flat_map(move |line| {
        grid[line][..]
            .iter()
            .enumerate()
            .filter_map(move |(column, cell)| {
                (!cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
                .then_some((Point::new(line, Column(column)), cell))
            })
    })
}

pub(super) fn cell_chars(cell: &Cell) -> impl Iterator<Item = char> + '_ {
    std::iter::once(cell.c).chain(cell.zerowidth().unwrap_or_default().iter().copied())
}

fn spans(text: &str) -> Option<(Option<Range<usize>>, Range<usize>)> {
    if let Some(path) = text
        .strip_prefix("PS ")
        .and_then(|text| text.strip_suffix('>'))
    {
        let path = path.trim_end_matches('>');
        if path.starts_with('/') || windows_path(path) {
            return Some((None, 3..3 + path.len()));
        }
    }
    if let Some(path) = text.strip_suffix('>')
        && windows_path(path)
    {
        return Some((None, 0..path.len()));
    }

    let body = text.strip_suffix(['$', '#', '%', '❯'])?.trim_end();
    let mut shell_prompt = body;
    // Conda, virtualenv and some distributions add a parenthesized label to PS1.
    while let Some((label, rest)) = shell_prompt
        .strip_prefix('(')
        .and_then(|text| text.split_once(") "))
        && !label.trim().is_empty()
        && !label.contains(['(', ')'])
    {
        shell_prompt = rest.trim_start();
    }
    let prefix = body.len() - shell_prompt.len();
    let (bracketed, body) = if let Some(bracketed) = shell_prompt.strip_prefix('[') {
        (true, bracketed.strip_suffix(']')?)
    } else {
        (false, shell_prompt)
    };
    let (identity, path) = if bracketed {
        body.split_once(' ')
    } else {
        body.split_once(':').or_else(|| body.split_once(' '))
    }?;
    let (user, host) = identity.split_once('@')?;
    let valid_name = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
    };
    let path = path.trim_start();
    if !valid_name(user)
        || !valid_name(host)
        || path.is_empty()
        || (!bracketed
            && !path.starts_with(['/', '~'])
            && path
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, ':' | '@')))
    {
        return None;
    }
    let start = prefix + usize::from(bracketed);
    let end = start + body.len();
    Some((Some(start..start + identity.len()), end - path.len()..end))
}

fn windows_path(path: &str) -> bool {
    path.starts_with("\\\\")
        || matches!(path.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}
