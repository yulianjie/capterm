use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::{Column, Line},
    term::cell::{Cell, Flags},
};

#[derive(Default)]
pub(super) struct Timeline {
    rows: HashMap<usize, Stamp>,
    last_history: Option<usize>,
}

struct Stamp {
    text_hash: u64,
    time: Option<Arc<str>>,
}

impl Timeline {
    pub(super) fn output(&mut self, grid: &Grid<Cell>, time: Arc<str>) {
        // Alacritty moves owned row buffers when the grid scrolls. Use the
        // backing buffer as an in-process key and validate the text before
        // reusing a stamp. The key is never dereferenced or exposed.
        for line in 0..grid.screen_lines() as i32 {
            self.track(grid, Line(line), Some(&time));
        }

        for line in (grid.topmost_line().0..0).rev().map(Line) {
            if Some(row_key(grid, line)) == self.last_history {
                break;
            }
            self.track(grid, line, Some(&time));
        }
        self.update_history_anchor(grid);

        // Avoid scanning unchanged history on every output batch. The map can
        // temporarily contain a small number of evicted backing buffers.
        if self.rows.len() > grid.total_lines().saturating_add(128) {
            let live: HashSet<_> = (grid.topmost_line().0..grid.screen_lines() as i32)
                .map(|line| row_key(grid, Line(line)))
                .collect();
            self.rows.retain(|key, _| live.contains(key));
        }
    }

    pub(super) fn resized(&mut self, grid: &Grid<Cell>) {
        // Reflow may replace or repurpose backing buffers. Keep unambiguous
        // stamps and leave a blank where the original row cannot be traced.
        for line in 0..grid.screen_lines() as i32 {
            self.track(grid, Line(line), None);
        }
        self.update_history_anchor(grid);
    }

    pub(super) fn time(&self, grid: &Grid<Cell>, line: Line) -> Option<Arc<str>> {
        let stamp = self.rows.get(&row_key(grid, line))?;
        (stamp.text_hash == row_text_hash(grid, line)?)
            .then(|| stamp.time.clone())
            .flatten()
    }

    fn track(&mut self, grid: &Grid<Cell>, line: Line, received: Option<&Arc<str>>) {
        let key = row_key(grid, line);
        let Some(text_hash) = row_text_hash(grid, line) else {
            self.rows.remove(&key);
            return;
        };
        if self
            .rows
            .get(&key)
            .is_some_and(|stamp| stamp.text_hash == text_hash)
        {
            return;
        }
        self.rows.insert(
            key,
            Stamp {
                text_hash,
                time: received.cloned(),
            },
        );
    }

    fn update_history_anchor(&mut self, grid: &Grid<Cell>) {
        self.last_history = (grid.history_size() > 0).then(|| row_key(grid, Line(-1)));
    }
}

fn row_key(grid: &Grid<Cell>, line: Line) -> usize {
    (&grid[line][Column(0)] as *const Cell).addr()
}

fn row_text_hash(grid: &Grid<Cell>, line: Line) -> Option<u64> {
    let mut hash = DefaultHasher::new();
    let mut has_content = false;
    for (column, cell) in grid[line][..].iter().enumerate() {
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        if cell.c != ' ' || cell.zerowidth().is_some_and(|chars| !chars.is_empty()) {
            column.hash(&mut hash);
            cell.c.hash(&mut hash);
            cell.zerowidth().hash(&mut hash);
            has_content = true;
        }
    }
    has_content.then(|| hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::{Term, event::VoidListener, term::Config, vte::ansi::Processor};

    use crate::terminal::TerminalSize;

    #[test]
    fn timestamps_follow_scrolled_rows_after_history_fills() {
        let size = TerminalSize {
            rows: 2,
            cols: 8,
            ..TerminalSize::default()
        };
        let config = Config {
            scrolling_history: 2,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, VoidListener);
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        for (text, time) in [
            (b"one".as_slice(), "09:00:01"),
            (b"\r\ntwo", "09:00:02"),
            (b"\r\nthree", "09:00:03"),
            (b"\r\nfour", "09:00:04"),
            (b"\r\nfive", "09:00:05"),
        ] {
            parser.advance(&mut term, text);
            timeline.output(term.grid(), Arc::from(time));
        }
        assert_eq!(term.grid().history_size(), 2);
        assert_eq!(
            timeline.time(term.grid(), Line(-2)).as_deref(),
            Some("09:00:02")
        );
        assert_eq!(
            timeline.time(term.grid(), Line(-1)).as_deref(),
            Some("09:00:03")
        );
        assert_eq!(
            timeline.time(term.grid(), Line(0)).as_deref(),
            Some("09:00:04")
        );
        assert_eq!(
            timeline.time(term.grid(), Line(1)).as_deref(),
            Some("09:00:05")
        );
    }

    #[test]
    fn cursor_movement_does_not_change_output_time() {
        let mut term = Term::new(Config::default(), &TerminalSize::default(), VoidListener);
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        parser.advance(&mut term, b"progress");
        timeline.output(term.grid(), Arc::from("09:00:01"));
        parser.advance(&mut term, b"\r");
        timeline.output(term.grid(), Arc::from("09:00:02"));
        assert_eq!(
            timeline.time(term.grid(), Line(0)).as_deref(),
            Some("09:00:01")
        );
        parser.advance(&mut term, b"finished");
        timeline.output(term.grid(), Arc::from("09:00:03"));
        assert_eq!(
            timeline.time(term.grid(), Line(0)).as_deref(),
            Some("09:00:03")
        );
    }

    #[test]
    fn recycled_rows_with_identical_text_get_fresh_timestamps() {
        let size = TerminalSize {
            rows: 2,
            cols: 8,
            ..TerminalSize::default()
        };
        let config = Config {
            scrolling_history: 2,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, VoidListener);
        let mut parser: Processor = Processor::new();
        let mut timeline = Timeline::default();
        parser.advance(&mut term, b"same");
        timeline.output(term.grid(), Arc::from("09:00:01"));
        for sequence in 2..=12 {
            parser.advance(&mut term, b"\r\nsame");
            let time: Arc<str> = Arc::from(format!("09:00:{sequence:02}"));
            timeline.output(term.grid(), time.clone());
            assert_eq!(timeline.time(term.grid(), Line(1)), Some(time));
        }
    }
}
