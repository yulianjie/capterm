use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
    hash::{DefaultHasher, Hash, Hasher},
    num::NonZeroU64,
    time::Duration,
};

use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::{Column, Line},
    term::cell::{Cell, Flags, LineLength},
};
use chrono::{DateTime, Local};

use super::prompt::{Prompt, PromptColors};

#[derive(Clone, Copy, Default)]
pub enum TimestampFormat {
    #[default]
    Seconds,
    Milliseconds,
    Relative,
}

#[derive(Default)]
pub(super) struct Timeline {
    rows: HashMap<usize, Stamp>,
    logical_times: HashMap<u64, DateTime<Local>>,
    last_history: Option<usize>,
    next_number: u64,
    start: Option<DateTime<Local>>,
    pub(super) commands: BTreeMap<u64, CommandBlock>,
    active_command: Option<u64>,
    has_shell_markers: bool,
    pending_prompt: Option<(u64, usize)>,
    pub(super) systems: BTreeMap<u64, Vec<SystemLine>>,
}

pub(super) struct SystemLine {
    pub(super) time: DateTime<Local>,
    pub(super) text: String,
}

#[derive(Clone, Copy)]
pub(super) struct Stamp {
    pub(super) number: u64,
    pub(super) time: DateTime<Local>,
    pub(super) live: bool,
    text_hash: Option<NonZeroU64>,
}

pub(super) struct CommandBlock {
    started: Option<DateTime<Local>>,
    pub(super) elapsed: Option<Duration>,
    pub(super) status: Option<i32>,
    pub(super) last_line: Option<u64>,
    pub(super) collapsed: bool,
    prompt: Option<Prompt>,
}

impl CommandBlock {
    fn new() -> Self {
        Self {
            started: None,
            elapsed: None,
            status: None,
            last_line: None,
            collapsed: false,
            prompt: None,
        }
    }

    pub(super) fn running(&self) -> bool {
        self.started.is_some() && self.last_line.is_none()
    }
}

impl Timeline {
    pub(super) fn latest_prompt(&self) -> Option<u64> {
        self.commands.last_key_value().map(|(&number, _)| number)
    }

    pub(super) fn captured_prompt(&self, number: u64) -> Option<&str> {
        Some(self.commands.get(&number)?.prompt.as_ref()?.text())
    }

    pub(super) fn touch(
        &mut self,
        grid: &Grid<Cell>,
        line: Line,
        time: DateTime<Local>,
        fresh: bool,
        live: bool,
    ) {
        self.start.get_or_insert(time);
        let key = row_key(grid, line);
        let previous = (!fresh).then(|| self.rows.get(&key)).flatten();
        let number = if let Some(continuation) = self.continued_from(grid, line) {
            continuation.number
        } else if let Some(previous) = previous {
            previous.number
        } else {
            self.next_number += 1;
            self.next_number
        };
        if previous.is_some_and(|stamp| {
            stamp.number == number
                && stamp.time == time
                && stamp.live == live
                && stamp.text_hash.is_none()
        }) {
            return;
        }
        self.logical_times.insert(number, time);
        self.rows.insert(
            key,
            Stamp {
                number,
                time,
                live,
                text_hash: None,
            },
        );
    }

    pub(super) fn ensure(&mut self, grid: &Grid<Cell>, line: Line, time: DateTime<Local>) {
        if self.stamp(grid, line).is_none() {
            self.touch(grid, line, time, false, false);
        }
    }

    pub(super) fn output(&mut self, grid: &Grid<Cell>, time: DateTime<Local>) {
        let mut history_start = Line(0);
        for line in (grid.topmost_line().0..0).rev().map(Line) {
            if Some(row_key(grid, line)) == self.last_history {
                break;
            }
            history_start = line;
        }
        for line in (history_start.0..grid.screen_lines() as i32).map(Line) {
            let key = row_key(grid, line);
            let hash = row_text_hash(grid, line);
            if let Some(stamp) = self.rows.get_mut(&key) {
                if stamp.text_hash.is_some() && stamp.text_hash != hash {
                    stamp.time = time;
                    self.logical_times.insert(stamp.number, time);
                    stamp.live = line.0 >= 0 && hash.is_some();
                }
                stamp.text_hash = hash;
                if line.0 < 0 {
                    stamp.live = false;
                }
            } else if hash.is_some() {
                self.touch(grid, line, time, false, false);
                if let Some(stamp) = self.rows.get_mut(&key) {
                    stamp.text_hash = hash;
                }
            }
        }
        self.update_history_anchor(grid);
        // A cleanup scans the entire grid. Keep it infrequent when the
        // history is large and rows are being recycled at a steady rate.
        let cleanup_slack = (grid.total_lines() / 8).max(1024);
        if self.rows.len() > grid.total_lines().saturating_add(128)
            || self.logical_times.len() > self.rows.len().saturating_add(cleanup_slack)
        {
            self.retain_live(grid);
        }
        if !self.has_shell_markers {
            self.detect_prompt(grid, time);
        }
    }

    pub(super) fn clear_reused(&mut self, grid: &Grid<Cell>, line: Line) {
        self.rows.remove(&row_key(grid, line));
    }

    pub(super) fn reset(&mut self) {
        *self = Self {
            next_number: self.next_number,
            start: self.start,
            ..Self::default()
        };
    }

    pub(super) fn stamp(&self, grid: &Grid<Cell>, line: Line) -> Option<Stamp> {
        let mut stamp = *self.rows.get(&row_key(grid, line))?;
        if let Some(&time) = self.logical_times.get(&stamp.number) {
            stamp.time = time;
        }
        Some(stamp)
    }

    pub(super) fn continued_from(&self, grid: &Grid<Cell>, line: Line) -> Option<Stamp> {
        let previous = line - 1;
        (previous >= grid.topmost_line() && wraps(grid, previous))
            .then(|| self.stamp(grid, previous))
            .flatten()
    }

    pub(super) fn before_resize(&self, grid: &Grid<Cell>) -> Vec<Option<Stamp>> {
        let mut logical = Vec::new();
        let mut previous_wrapped = false;
        for line in (grid.topmost_line().0..grid.screen_lines() as i32).map(Line) {
            let stamp = self.stamp(grid, line);
            if previous_wrapped {
                if let Some(stamp) = stamp {
                    let last: &mut Option<Stamp> =
                        logical.last_mut().expect("previous logical line");
                    if last.is_none_or(|previous| stamp.time > previous.time) {
                        *last = Some(stamp);
                    }
                }
            } else {
                logical.push(stamp);
            }
            previous_wrapped = wraps(grid, line);
        }
        while logical.last().is_some_and(Option::is_none) {
            logical.pop();
        }
        logical
    }

    pub(super) fn resized(&mut self, grid: &Grid<Cell>, logical: Vec<Option<Stamp>>) {
        let mut groups = Vec::new();
        let mut start = grid.topmost_line();
        let mut last_used = grid.cursor.point.line;
        for line in (grid.topmost_line().0..grid.screen_lines() as i32).map(Line) {
            if grid[line][..].iter().any(has_text) {
                last_used = last_used.max(line);
            }
            if !wraps(grid, line) {
                groups.push((start, line));
                start = line + 1;
            }
        }
        // Preserve chronological metadata through terminal reflow. Empty screen
        // rows below the cursor are spare capacity and are never numbered.
        let meaningful = groups.partition_point(|(start, _)| *start <= last_used);
        let offset = logical.len().saturating_sub(meaningful);
        self.rows.clear();
        self.logical_times.clear();
        for ((start, end), stamp) in groups.into_iter().zip(logical.into_iter().skip(offset)) {
            if let Some(stamp) = stamp {
                self.logical_times.insert(stamp.number, stamp.time);
                for line in (start.0..=end.0).map(Line) {
                    self.rows.insert(
                        row_key(grid, line),
                        Stamp {
                            text_hash: row_text_hash(grid, line),
                            ..stamp
                        },
                    );
                }
            }
        }
        self.update_history_anchor(grid);
    }

    pub(super) fn formatted_log(&self, grid: &Grid<Cell>, format: TimestampFormat) -> String {
        let mut log = String::new();
        let mut last_number = None;
        let mut last_timestamp: Option<(DateTime<Local>, String)> = None;
        for line in (grid.topmost_line().0..=grid.bottommost_line().0).map(Line) {
            if let Some(stamp) = self.stamp(grid, line) {
                let continuation = last_number == Some(stamp.number);
                if !continuation {
                    if let Some(messages) = self.systems.get(&stamp.number) {
                        for message in messages {
                            writeln!(
                                log,
                                "· {}  {}",
                                self.timestamp(message.time, format),
                                message.text
                            )
                            .unwrap();
                        }
                    }
                    if last_timestamp.as_ref().is_none_or(|(time, _)| {
                        *time != stamp.time || time.offset() != stamp.time.offset()
                    }) {
                        last_timestamp = Some((stamp.time, self.timestamp(stamp.time, format)));
                    }
                    let timestamp = &last_timestamp.as_ref().unwrap().1;
                    write!(log, "{} {timestamp}  ", stamp.number).unwrap();
                }
                last_number = Some(stamp.number);
                append_row_text(&mut log, grid, line);
                if !wraps(grid, line) {
                    log.push('\n');
                }
            }
        }
        for message in self.pending_systems() {
            writeln!(
                log,
                "· {}  {}",
                self.timestamp(message.time, format),
                message.text
            )
            .unwrap();
        }
        log
    }

    pub(super) fn timestamp(&self, time: DateTime<Local>, format: TimestampFormat) -> String {
        match format {
            TimestampFormat::Seconds => time.format("%H:%M:%S").to_string(),
            TimestampFormat::Milliseconds => time.format("%H:%M:%S%.3f").to_string(),
            TimestampFormat::Relative => {
                let seconds = self
                    .start
                    .map_or(0, |start| (time - start).num_seconds().max(0));
                format!(
                    "+{:02}:{:02}:{:02}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                )
            }
        }
    }

    pub(super) fn prompt(&mut self, grid: &Grid<Cell>, time: DateTime<Local>) {
        self.has_shell_markers = true;
        self.pending_prompt = None;
        let line = grid.cursor.point.line;
        self.ensure(grid, line, time);
        if let Some(stamp) = self.stamp(grid, line) {
            if let Some(last) = self
                .active_command
                .take()
                .and_then(|number| self.commands.get_mut(&number))
                && last.last_line.is_none()
            {
                last.last_line = Some(stamp.number.saturating_sub(1));
            }
            self.commands
                .entry(stamp.number)
                .or_insert_with(CommandBlock::new);
            self.active_command = Some(stamp.number);
            if self.continued_from(grid, line).is_none() && !grid.cursor.input_needs_wrap {
                self.pending_prompt = Some((stamp.number, grid.cursor.point.column.0));
            }
        }
    }

    pub(super) fn end_prompt(&mut self, grid: &Grid<Cell>) {
        let Some((number, column)) = self.pending_prompt.take() else {
            return;
        };
        let line = grid.cursor.point.line;
        if self
            .stamp(grid, line)
            .is_some_and(|stamp| stamp.number == number)
        {
            let first = self.logical_start(grid, line, number);
            if let Some(block) = self.commands.get_mut(&number) {
                block.prompt = Prompt::capture(grid, first, column);
            }
        }
    }

    pub(super) fn prompt_colors(
        &self,
        grid: &Grid<Cell>,
        line: Line,
        number: u64,
    ) -> Option<PromptColors> {
        let prompt = self.commands.get(&number)?.prompt.as_ref()?;
        prompt.colors(grid, self.logical_start(grid, line, number))
    }

    fn logical_start(&self, grid: &Grid<Cell>, mut line: Line, number: u64) -> Line {
        while self
            .continued_from(grid, line)
            .is_some_and(|stamp| stamp.number == number)
        {
            line -= 1;
        }
        line
    }

    pub(super) fn start_command(&mut self, time: DateTime<Local>) {
        self.has_shell_markers = true;
        self.pending_prompt = None;
        if let Some(block) = self
            .active_command
            .and_then(|number| self.commands.get_mut(&number))
        {
            block.started = Some(time);
        }
    }

    pub(super) fn start_fallback(&mut self, time: DateTime<Local>) {
        if !self.has_shell_markers
            && let Some(block) = self
                .active_command
                .and_then(|number| self.commands.get_mut(&number))
        {
            block.started.get_or_insert(time);
        }
    }

    pub(super) fn finish_command(
        &mut self,
        grid: &Grid<Cell>,
        time: DateTime<Local>,
        status: Option<i32>,
    ) {
        self.has_shell_markers = true;
        self.pending_prompt = None;
        let line = grid.cursor.point.line;
        let last_line = self.stamp(grid, line).or_else(|| {
            (line > grid.topmost_line())
                .then(|| self.stamp(grid, line - 1))
                .flatten()
        });
        if let Some((number, block)) = self
            .active_command
            .take()
            .and_then(|number| self.commands.get_mut(&number).map(|block| (number, block)))
        {
            block.status = status;
            block.elapsed = block
                .started
                .and_then(|started| (time - started).to_std().ok());
            block.last_line = Some(last_line.map_or(number, |stamp| stamp.number).max(number));
        }
    }

    pub(super) fn command_for(&self, number: u64) -> Option<(u64, &CommandBlock)> {
        let (&start, block) = self.commands.range(..=number).next_back()?;
        (start == number || block.last_line.is_some_and(|last| number <= last) || block.running())
            .then_some((start, block))
    }

    pub(super) fn system_at_cursor(
        &mut self,
        grid: &Grid<Cell>,
        time: DateTime<Local>,
        message: String,
    ) {
        let line = grid.cursor.point.line;
        let next = self
            .stamp(grid, line)
            .map_or(self.next_number + 1, |stamp| stamp.number);
        self.system_before(next, time, message);
    }

    pub(super) fn system_after_output(&mut self, time: DateTime<Local>, message: String) {
        self.system_before(self.next_number + 1, time, message);
    }

    pub(super) fn pending_systems(&self) -> &[SystemLine] {
        self.systems
            .get(&(self.next_number + 1))
            .map_or(&[], Vec::as_slice)
    }

    fn system_before(&mut self, next: u64, time: DateTime<Local>, message: String) {
        self.start.get_or_insert(time);
        self.systems.entry(next).or_default().push(SystemLine {
            time,
            text: message,
        });
    }

    fn detect_prompt(&mut self, grid: &Grid<Cell>, time: DateTime<Local>) {
        let line = grid.cursor.point.line;
        let Some(stamp) = self.stamp(grid, line) else {
            return;
        };
        let text = row_text(grid, line);
        let prompt = text.trim_end();
        let cursor = grid.cursor.point.column.0 + usize::from(grid.cursor.input_needs_wrap);
        let has_prompt_suffix = prompt.ends_with(['$', '#', '❯', '%', '>']);
        let styled_prompt = if has_prompt_suffix
            && self
                .commands
                .get(&stamp.number)
                .is_none_or(|block| block.prompt.is_none())
            && grid[line][Column(cursor)..]
                .iter()
                .all(|cell| cell.c == ' ')
        {
            Prompt::capture(grid, self.logical_start(grid, line, stamp.number), 0)
        } else {
            None
        };
        if styled_prompt.is_some()
            || is_powershell_prompt(prompt)
            || has_prompt_suffix
                && prompt.split_whitespace().next().is_some_and(|part| {
                    part.contains('@')
                        || part.contains(':')
                        || part == "❯"
                        || part == "$"
                        || part == "%"
                })
        {
            if let Some(previous) = self
                .active_command
                .filter(|&previous| previous < stamp.number)
                && let Some(previous) = self.commands.get_mut(&previous)
            {
                previous.last_line = Some(stamp.number - 1);
                previous.elapsed = previous
                    .started
                    .and_then(|started| (time - started).to_std().ok());
            }
            let block = self
                .commands
                .entry(stamp.number)
                .or_insert_with(CommandBlock::new);
            if let Some(prompt) = styled_prompt {
                block.prompt = Some(prompt);
            }
            self.active_command = Some(stamp.number);
        }
    }

    fn update_history_anchor(&mut self, grid: &Grid<Cell>) {
        self.last_history = (grid.history_size() > 0).then(|| row_key(grid, Line(-1)));
    }

    fn retain_live(&mut self, grid: &Grid<Cell>) {
        let live: HashSet<_> = (grid.topmost_line().0..grid.screen_lines() as i32)
            .map(|line| row_key(grid, Line(line)))
            .collect();
        self.rows.retain(|key, _| live.contains(key));
        let oldest = self
            .rows
            .values()
            .map(|stamp| stamp.number)
            .min()
            .unwrap_or_default();
        if self.logical_times.len() > self.rows.len().saturating_add(128) {
            self.logical_times.retain(|number, _| *number >= oldest);
        }
        self.commands
            .retain(|number, block| block.last_line.unwrap_or(*number) >= oldest);
        self.systems.retain(|number, _| *number >= oldest);
    }
}

pub(super) fn is_powershell_prompt(text: &str) -> bool {
    let Some(path) = text
        .trim_end()
        .strip_prefix("PS ")
        .and_then(|text| text.strip_suffix('>'))
    else {
        return false;
    };
    let path = path.trim_end_matches('>');
    path.starts_with(['/', '\\'])
        || path
            .split_once(':')
            .is_some_and(|(drive, path)| !drive.is_empty() && path.starts_with(['/', '\\']))
}

pub(super) fn row_key(grid: &Grid<Cell>, line: Line) -> usize {
    // Alacritty moves owned row buffers when scrolling. The address is only a key.
    (&grid[line][Column(0)] as *const Cell).addr()
}

pub(super) fn row_text(grid: &Grid<Cell>, line: Line) -> String {
    let mut result = String::new();
    append_row_text(&mut result, grid, line);
    result
}

fn append_row_text(result: &mut String, grid: &Grid<Cell>, line: Line) {
    let start = result.len();
    let row = &grid[line];
    for cell in &row[..row.line_length()] {
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        result.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            result.extend(extra);
        }
    }
    result.truncate(start + result[start..].trim_end().len());
}

pub(super) fn wraps(grid: &Grid<Cell>, line: Line) -> bool {
    grid[line][grid.last_column()]
        .flags
        .contains(Flags::WRAPLINE)
}

fn has_text(cell: &Cell) -> bool {
    !cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        && (cell.c != ' ' || cell.zerowidth().is_some_and(|chars| !chars.is_empty()))
}

fn row_text_hash(grid: &Grid<Cell>, line: Line) -> Option<NonZeroU64> {
    let mut hash = DefaultHasher::new();
    let mut has_content = false;
    for (column, cell) in grid[line][..].iter().enumerate() {
        if has_text(cell) {
            let extra = cell.zerowidth();
            hash.write_u64(
                (column as u64) << 32 | u64::from(cell.c) << 1 | u64::from(extra.is_some()),
            );
            if let Some(chars) = extra {
                chars.hash(&mut hash);
            }
            has_content = true;
        }
    }
    has_content.then(|| NonZeroU64::new(hash.finish().max(1)).unwrap())
}
