use std::{collections::BTreeMap, fs::File, io::Read, ops::Range, path::Path};

use anyhow::{Context, Result, bail};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;

pub(crate) const BUNDLED: &str = include_str!("../../assets/terminal/linux-highlights.json");

const MAX_CONFIG_BYTES: usize = 128 * 1024;
const MAX_RULES: usize = 128;
const MAX_PATTERN_BYTES: usize = 4096;
const MAX_CAPTURE_GROUPS: usize = 32;
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

pub(crate) struct LinuxHighlights {
    rules: Vec<Rule>,
}

struct Rule {
    scope: Scope,
    regex: Regex,
    captures: BTreeMap<usize, Ink>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Prompt,
    Command,
    Output,
    Both,
}

impl Scope {
    fn includes(self, domain: Domain) -> bool {
        match (self, domain) {
            (Self::Prompt, Domain::Prompt) => true,
            (Self::Command, Domain::Command) => true,
            (Self::Output, Domain::Output) => true,
            (Self::Both, Domain::Prompt) => false,
            (Self::Both, Domain::Command | Domain::Output) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ink {
    Ansi(usize),
    Rgb(u32),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    rules: Vec<RuleSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSpec {
    scope: Scope,
    pattern: String,
    captures: BTreeMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Domain {
    Prompt,
    Command,
    Output,
}

#[derive(Default)]
struct Domains {
    prompt: Option<Range<usize>>,
    command: Option<Range<usize>>,
    output: Option<Range<usize>>,
}

impl Domains {
    fn get(&self, domain: Domain) -> Option<&Range<usize>> {
        match domain {
            Domain::Prompt => self.prompt.as_ref(),
            Domain::Command => self.command.as_ref(),
            Domain::Output => self.output.as_ref(),
        }
    }
}

impl LinuxHighlights {
    pub(crate) fn bundled() -> Self {
        Self::parse(BUNDLED).expect("bundled Linux terminal highlight rules are valid")
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::bundled());
            }
            Err(error) => return Err(error).with_context(|| format!("Opening {}", path.display())),
        };
        let mut source = String::new();
        file.by_ref()
            .take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_string(&mut source)
            .with_context(|| format!("Reading {}", path.display()))?;
        if source.len() > MAX_CONFIG_BYTES {
            bail!(
                "{} exceeds the 128 KiB highlight configuration limit",
                path.display()
            );
        }
        Self::parse(&source).with_context(|| format!("Parsing {}", path.display()))
    }

    fn parse(source: &str) -> Result<Self> {
        if source.len() > MAX_CONFIG_BYTES {
            bail!("highlight configuration exceeds the 128 KiB limit");
        }
        let config: Config = serde_json::from_str(source).context("Invalid highlight JSON")?;
        if config.rules.len() > MAX_RULES {
            bail!("highlight configuration has more than {MAX_RULES} rules");
        }

        let mut rules = Vec::with_capacity(config.rules.len());
        for (index, spec) in config.rules.into_iter().enumerate() {
            if spec.pattern.len() > MAX_PATTERN_BYTES {
                bail!(
                    "rule {} pattern exceeds {MAX_PATTERN_BYTES} bytes",
                    index + 1
                );
            }
            let regex = RegexBuilder::new(&spec.pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .nest_limit(64)
                .build()
                .with_context(|| format!("Invalid regex in rule {}", index + 1))?;
            if regex.captures_len() > MAX_CAPTURE_GROUPS {
                bail!(
                    "rule {} has more than {} capture groups",
                    index + 1,
                    MAX_CAPTURE_GROUPS - 1
                );
            }

            let mut captures = BTreeMap::new();
            for (capture, color) in spec.captures {
                let capture = capture.parse::<usize>().with_context(|| {
                    format!("rule {} has non-numeric capture key {capture:?}", index + 1)
                })?;
                if capture >= regex.captures_len() {
                    bail!(
                        "rule {} refers to missing capture group {capture}",
                        index + 1
                    );
                }
                captures.insert(
                    capture,
                    parse_ink(&color).with_context(|| {
                        format!("rule {} has invalid color for capture {capture}", index + 1)
                    })?,
                );
            }
            rules.push(Rule {
                scope: spec.scope,
                regex,
                captures,
            });
        }
        Ok(Self { rules })
    }

    pub(super) fn colors(
        &self,
        text: &str,
        known_prompt: Option<Range<usize>>,
    ) -> Vec<Option<Ink>> {
        let mut colors = vec![None; text.len()];
        if text.is_empty() || self.rules.is_empty() {
            return colors;
        }
        let domains = domains(text, known_prompt);
        for rule in &self.rules {
            for domain in [Domain::Prompt, Domain::Command, Domain::Output] {
                if !rule.scope.includes(domain) {
                    continue;
                }
                let Some(range) = domains.get(domain) else {
                    continue;
                };
                let Some(subject) = text.get(range.clone()) else {
                    continue;
                };
                for found in rule.regex.captures_iter(subject) {
                    // Higher-numbered captures are the inner captures. Fill them first so
                    // the whole-match capture can provide a color around the inner span.
                    for (&group, &ink) in rule.captures.iter().rev() {
                        let Some(found) = found.get(group) else {
                            continue;
                        };
                        let start = range.start + found.start();
                        let end = range.start + found.end();
                        for color in &mut colors[start..end] {
                            color.get_or_insert(ink);
                        }
                    }
                }
            }
        }
        colors
    }
}

fn parse_ink(color: &str) -> Result<Ink> {
    let ansi = match color {
        "black" => Some(0),
        "red" => Some(1),
        "green" => Some(2),
        "yellow" => Some(3),
        "blue" => Some(4),
        "magenta" => Some(5),
        "cyan" => Some(6),
        "white" => Some(7),
        "bright_black" => Some(8),
        "bright_red" => Some(9),
        "bright_green" => Some(10),
        "bright_yellow" => Some(11),
        "bright_blue" => Some(12),
        "bright_magenta" => Some(13),
        "bright_cyan" => Some(14),
        "bright_white" => Some(15),
        _ => None,
    };
    if let Some(index) = ansi {
        return Ok(Ink::Ansi(index));
    }

    let Some(rgb) = color.strip_prefix('#').filter(|value| value.len() == 6) else {
        bail!("expected an ANSI color name or #RRGGBB value");
    };
    Ok(Ink::Rgb(
        u32::from_str_radix(rgb, 16).context("Invalid RGB color")?,
    ))
}

fn domains(text: &str, known_prompt: Option<Range<usize>>) -> Domains {
    if let Some(prompt) = known_prompt.filter(|range| {
        range.start <= range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
    }) {
        let command = trim_start(text, prompt.end..text.len());
        return Domains {
            output: non_empty(0..prompt.start),
            prompt: non_empty(trim_end(text, prompt)),
            command: non_empty(command),
        };
    }

    if let Some((prompt_end, command_start)) = generic_prompt(text) {
        return Domains {
            prompt: non_empty(trim_end(text, 0..prompt_end)),
            command: non_empty(trim_start(text, command_start..text.len())),
            output: None,
        };
    }

    Domains {
        output: non_empty(0..text.len()),
        ..Domains::default()
    }
}

fn trim_start(text: &str, range: Range<usize>) -> Range<usize> {
    let Some(value) = text.get(range.clone()) else {
        return range;
    };
    let trimmed = value.trim_start_matches(char::is_whitespace);
    range.start + value.len() - trimmed.len()..range.end
}

fn trim_end(text: &str, range: Range<usize>) -> Range<usize> {
    let Some(value) = text.get(range.clone()) else {
        return range;
    };
    range.start..range.start + value.trim_end_matches(char::is_whitespace).len()
}

fn non_empty(range: Range<usize>) -> Option<Range<usize>> {
    (range.start < range.end).then_some(range)
}

fn generic_prompt(text: &str) -> Option<(usize, usize)> {
    let visible = text.trim_end_matches(char::is_whitespace);
    let visible_len = visible.len();
    let first = visible
        .trim_start_matches(char::is_whitespace)
        .chars()
        .next()?;
    if matches!(first, '+' | '-') {
        return None;
    }

    let mut candidate = None;
    for (offset, ch) in visible.char_indices() {
        let next = visible[offset + ch.len_utf8()..].chars().next();
        if is_prompt_delimiter(ch)
            && next.is_none_or(char::is_whitespace)
            && !(ch == '%' && numeric_percent(visible, offset))
        {
            candidate.get_or_insert((offset + ch.len_utf8(), offset + ch.len_utf8()));
        }
    }
    let (prompt_end, command_start) = candidate?;
    Some((prompt_end, command_start.min(visible_len)))
}

fn numeric_percent(text: &str, offset: usize) -> bool {
    let token = text[..offset].rsplit(char::is_whitespace).next();
    let Some(token) = token else {
        return false;
    };
    let mut parts = token.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|fraction| {
            !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
        && parts.next().is_none()
}

fn is_prompt_delimiter(ch: char) -> bool {
    matches!(ch, '$' | '#' | '%' | '>' | '❯' | '\u{e0b0}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> LinuxHighlights {
        LinuxHighlights::parse(source).unwrap()
    }

    #[test]
    fn known_prompt_separates_output_prompt_and_command() {
        let highlights = rules(
            r##"{"rules":[
              {"scope":"output","pattern":"^(out)","captures":{"1":"red"}},
              {"scope":"prompt","pattern":"(user@host)","captures":{"1":"green"}},
              {"scope":"command","pattern":"^(ls)","captures":{"1":"#123456"}},
              {"scope":"both","pattern":".","captures":{"0":"blue"}}
            ]}"##,
        );
        let text = "out user@host:~$  ls";
        let overlays = highlights.colors(text, Some(4..17));
        assert_eq!(overlays[0], Some(Ink::Ansi(1)));
        assert_eq!(overlays[5], Some(Ink::Ansi(2)));
        assert_eq!(overlays[19], Some(Ink::Rgb(0x123456)));
        assert_eq!(overlays[17], None);
    }

    #[test]
    fn generic_prompt_guards_percent_and_diff_like_prefixes() {
        let highlights = rules(
            r##"{"rules":[
              {"scope":"prompt","pattern":"^(.+)","captures":{"1":"green"}},
              {"scope":"command","pattern":"^(.+)","captures":{"1":"blue"}},
              {"scope":"output","pattern":"^(.+)","captures":{"1":"red"}}
            ]}"##,
        );
        assert_eq!(
            highlights.colors("user@host$ ls", None)[0],
            Some(Ink::Ansi(2))
        );
        assert_eq!(highlights.colors("100%", None)[0], Some(Ink::Ansi(1)));
        assert_eq!(highlights.colors("100.5%", None)[0], Some(Ink::Ansi(1)));
        assert_eq!(
            highlights.colors("- diff > entry", None)[0],
            Some(Ink::Ansi(1))
        );
    }

    #[test]
    fn missing_configuration_uses_the_bundled_rules() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.json");
        let fallback = LinuxHighlights::load(&missing).unwrap();
        let bundled = LinuxHighlights::bundled();
        let text = "user@host$ ls --all";
        assert_eq!(fallback.colors(text, None), bundled.colors(text, None));
    }

    #[test]
    fn invalid_capture_and_color_are_rejected() {
        assert!(
            LinuxHighlights::parse(
                r##"{"rules":[{"scope":"prompt","pattern":"x","captures":{"2":"red"}}]}"##
            )
            .is_err()
        );
        assert!(
            LinuxHighlights::parse(
                r##"{"rules":[{"scope":"prompt","pattern":"x","captures":{"0":"chartreuse"}}]}"##
            )
            .is_err()
        );
    }
}
