use alacritty_terminal::{
    Term,
    grid::Dimensions,
    index::{Column, Direction, Point},
    term::search::{Match, RegexIter, RegexSearch},
};
use gpui_kit::{Entity, Subscription, Task, component::input::InputState};

use super::TerminalEvents;

const MAX_MATCHES: usize = 100_000;

#[derive(Default)]
pub(super) struct Search {
    pub(super) input: Option<(Entity<InputState>, Subscription)>,
    pub(super) open: bool,
    pub(super) regex: bool,
    pub(super) case_sensitive: bool,
    pub(super) matches: Vec<Match>,
    pub(super) current: usize,
    pub(super) pending: Option<Task<()>>,
    invalid: bool,
    truncated: bool,
}

impl Search {
    pub(super) fn update(&mut self, term: &Term<TerminalEvents>, query: &str, reset: bool) {
        let previous = (!reset)
            .then(|| self.matches.get(self.current))
            .flatten()
            .cloned();
        self.matches.clear();
        self.current = 0;
        self.invalid = false;
        self.truncated = false;
        if query.is_empty() {
            return;
        }
        let body = if self.regex {
            query.to_owned()
        } else {
            let mut escaped = String::new();
            for character in query.chars() {
                if "\\.^$|?*+()[]{}".contains(character) {
                    escaped.push('\\');
                }
                escaped.push(character);
            }
            escaped
        };
        let case = if self.case_sensitive { "-i" } else { "i" };
        let Ok(mut regex) = RegexSearch::new(&format!("(?{case}:{body})")) else {
            self.invalid = true;
            return;
        };
        let start = Point::new(term.topmost_line(), Column(0));
        let end = Point::new(term.bottommost_line(), term.last_column());
        let mut matches = RegexIter::new(start, end, Direction::Right, term, &mut regex);
        self.matches.extend(matches.by_ref().take(MAX_MATCHES));
        self.truncated = matches.next().is_some();
        self.current = previous.map_or_else(
            || {
                let top = -(term.grid().display_offset() as i32);
                let index = self.matches.partition_point(|hit| hit.start().line.0 < top);
                if index == self.matches.len() {
                    index.saturating_sub(1)
                } else {
                    index
                }
            },
            |previous| {
                self.matches
                    .partition_point(|hit| hit.start() < previous.start())
                    .min(self.matches.len().saturating_sub(1))
            },
        );
    }

    pub(super) fn label(&self) -> String {
        if self.invalid {
            return "正则无效".into();
        }
        let len = self.matches.len();
        let current = if len == 0 { 0 } else { self.current + 1 };
        let more = if self.truncated { "+" } else { "" };
        format!("{current} / {len}{more}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalSize;
    use alacritty_terminal::{term::Config, vte::ansi::Processor};

    #[test]
    fn case_and_regex_switches_control_matching_independently() {
        let mut term = Term::new(
            Config::default(),
            &TerminalSize::default(),
            TerminalEvents::default(),
        );
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"Warn WARN warn W.rn");
        let mut search = Search::default();
        search.update(&term, "WARN", true);
        assert_eq!(search.matches.len(), 3);
        search.case_sensitive = true;
        search.update(&term, "WARN", true);
        assert_eq!(search.matches.len(), 1);
        search.update(&term, "W.rn", true);
        assert_eq!(search.matches.len(), 1);
        search.regex = true;
        search.update(&term, "W.rn", true);
        assert_eq!(search.matches.len(), 2);
    }
}
