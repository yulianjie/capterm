use std::{collections::BTreeMap, sync::OnceLock};

use gpui_kit::{Keystroke, Modifiers, component::kbd::Kbd};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutAction {
    Palette,
    NewSession,
    DuplicateTab,
    CloseTab,
    NextTab,
    PreviousTab,
    SearchTerminal,
    FocusSender,
    ToggleGutters,
    ToggleFiles,
    ToggleSessions,
    ToggleTheme,
    FocusMode,
    SaveFile,
    OpenSettings,
    SenderHistory,
}

impl ShortcutAction {
    pub const ALL: [Self; 16] = [
        Self::Palette,
        Self::NewSession,
        Self::DuplicateTab,
        Self::CloseTab,
        Self::NextTab,
        Self::PreviousTab,
        Self::SearchTerminal,
        Self::FocusSender,
        Self::ToggleGutters,
        Self::ToggleFiles,
        Self::ToggleSessions,
        Self::ToggleTheme,
        Self::FocusMode,
        Self::SaveFile,
        Self::OpenSettings,
        Self::SenderHistory,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::NewSession => "new_session",
            Self::DuplicateTab => "duplicate_tab",
            Self::CloseTab => "close_tab",
            Self::NextTab => "next_tab",
            Self::PreviousTab => "previous_tab",
            Self::SearchTerminal => "search_terminal",
            Self::FocusSender => "focus_sender",
            Self::ToggleGutters => "toggle_gutters",
            Self::ToggleFiles => "toggle_files",
            Self::ToggleSessions => "toggle_sessions",
            Self::ToggleTheme => "toggle_theme",
            Self::FocusMode => "focus_mode",
            Self::SaveFile => "save_file",
            Self::OpenSettings => "open_settings",
            Self::SenderHistory => "sender_history",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Palette => "命令面板 / 快速连接",
            Self::NewSession => "新建会话",
            Self::DuplicateTab => "复制当前会话到新标签",
            Self::CloseTab => "关闭标签",
            Self::NextTab => "切换到下一个标签",
            Self::PreviousTab => "切换到上一个标签",
            Self::SearchTerminal => "终端内搜索",
            Self::FocusSender => "聚焦输入栏",
            Self::ToggleGutters => "行号与时间",
            Self::ToggleFiles => "文件面板",
            Self::ToggleSessions => "会话与监控面板",
            Self::ToggleTheme => "切换主题",
            Self::FocusMode => "专注模式",
            Self::SaveFile => "保存文件",
            Self::OpenSettings => "设置",
            Self::SenderHistory => "命令历史",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::SearchTerminal => "终端视图",
            Self::SaveFile => "文件编辑器",
            Self::SenderHistory => "命令输入栏获得焦点时",
            _ => "",
        }
    }

    fn default_chord(self) -> &'static Chord {
        static DEFAULTS: OnceLock<[Chord; ShortcutAction::ALL.len()]> = OnceLock::new();
        &DEFAULTS.get_or_init(|| Self::ALL.map(Self::make_default))[self as usize]
    }

    fn make_default(self) -> Chord {
        let mac = cfg!(target_os = "macos");
        let application = Modifiers {
            control: !mac,
            shift: !mac,
            platform: mac,
            ..Modifiers::default()
        };
        let primary = Modifiers {
            shift: false,
            ..application
        };
        let (key, modifiers) = match self {
            Self::Palette => ("p", application),
            Self::NewSession => ("n", application),
            Self::DuplicateTab => ("t", application),
            Self::CloseTab => ("w", application),
            Self::NextTab => ("tab", Modifiers::control()),
            Self::PreviousTab => ("tab", Modifiers::control_shift()),
            Self::SearchTerminal => ("f", application),
            Self::FocusSender => ("i", application),
            Self::ToggleGutters => ("g", application),
            Self::ToggleFiles => ("e", application),
            Self::ToggleSessions => ("b", application),
            Self::ToggleTheme => ("l", application),
            Self::FocusMode => ("enter", application),
            Self::SaveFile => ("s", primary),
            Self::OpenSettings => (",", primary),
            Self::SenderHistory => ("r", primary),
        };
        Chord {
            key: key.into(),
            modifiers,
        }
    }

    fn accepts(self, chord: &Chord) -> Result<(), &'static str> {
        let key = chord.key.as_str();
        let m = chord.modifiers;
        if chord == self.default_chord() {
            return Ok(());
        }
        if key == "escape" {
            return Err("Esc 用于取消录入或关闭对话框。");
        }
        if !(key.chars().count() == 1 && !key.chars().any(|c| c.is_whitespace() || c.is_control())
            || matches!(
                key,
                "backspace"
                    | "delete"
                    | "down"
                    | "end"
                    | "enter"
                    | "home"
                    | "insert"
                    | "left"
                    | "pagedown"
                    | "pageup"
                    | "right"
                    | "space"
                    | "tab"
                    | "up"
            )
            || key
                .strip_prefix('f')
                .and_then(|number| number.parse::<u8>().ok())
                .is_some_and(|number| (1..=24).contains(&number)))
        {
            return Err("请按一个普通键或 F1–F24，并加上修饰键。");
        }
        if cfg!(target_os = "macos") {
            let tab_switch =
                key == "tab" && (m == Modifiers::control() || m == Modifiers::control_shift());
            if tab_switch {
                return Ok(());
            }
            if !m.platform || m.control || m.alt {
                return Err("请使用 Command，或 Command+Shift。");
            }
            if matches!(key, "c" | "v" | "tab")
                || !m.shift && matches!(key, "h" | "m" | "q" | "space")
            {
                return Err("此组合键用于剪贴板或 macOS。");
            }
            if matches!(key, "f" | "up" | "down" | "left" | "right") {
                return Err("此组合键用于文本编辑。");
            }
        } else {
            let contextual = matches!(self, Self::SaveFile | Self::SenderHistory);
            if !m.control
                || m.alt
                || m.platform
                || !(m.shift || contextual || matches!(key, "tab" | ","))
            {
                return Err(if contextual {
                    "请使用 Ctrl 或 Ctrl+Shift。"
                } else {
                    "请使用 Ctrl+Shift。Ctrl+Tab 和 Ctrl+, 也可用于工作台。"
                });
            }
            if m.shift && matches!(key, "c" | "v") {
                return Err("此组合键用于终端剪贴板。");
            }
            if matches!(key, "left" | "right" | "home" | "end")
                || !m.shift && matches!(key, "backspace" | "delete" | "f" | "h")
            {
                return Err("此组合键用于文本编辑。");
            }
        }
        if !m.shift && matches!(key, "a" | "c" | "v" | "x" | "y" | "z") {
            return Err("此组合键用于文本编辑。");
        }
        if m.shift && key == "z" {
            return Err("此组合键用于文本编辑。");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Chord {
    modifiers: Modifiers,
    key: String,
}

impl Chord {
    fn modifiers(key: &Keystroke) -> Modifiers {
        // macOS also reports the Fn flag for some special keys.
        Modifiers {
            function: false,
            ..key.modifiers
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShortcutBindings(BTreeMap<String, Chord>);

impl ShortcutBindings {
    fn chord(&self, action: ShortcutAction) -> &Chord {
        self.0.get(action.id()).unwrap_or(action.default_chord())
    }

    pub fn keystroke(&self, action: ShortcutAction) -> Keystroke {
        let chord = self.chord(action);
        Keystroke {
            key: chord.key.clone(),
            modifiers: chord.modifiers,
            key_char: None,
        }
    }

    pub fn label(&self, action: ShortcutAction) -> String {
        Kbd::format(&self.keystroke(action))
    }

    pub fn with_label(&self, action: ShortcutAction, label: &str) -> String {
        format!("{label} · {}", self.label(action))
    }

    pub fn action(&self, key: &Keystroke) -> Option<ShortcutAction> {
        let modifiers = Chord::modifiers(key);
        if !modifiers.control && !modifiers.platform {
            return None;
        }
        ShortcutAction::ALL.into_iter().find(|&action| {
            let chord = self.chord(action);
            chord.modifiers == modifiers && chord.key.eq_ignore_ascii_case(&key.key)
        })
    }

    pub fn customized(&self, action: ShortcutAction) -> bool {
        self.0.contains_key(action.id())
    }

    pub fn has_customizations(&self) -> bool {
        !self.0.is_empty()
    }

    pub fn set(&mut self, action: ShortcutAction, key: &Keystroke) -> Result<(), String> {
        let chord = Chord {
            modifiers: Chord::modifiers(key),
            key: key.key.to_ascii_lowercase(),
        };
        self.replace(action, chord)
    }

    pub fn reset(&mut self, action: ShortcutAction) -> Result<(), String> {
        self.replace(action, action.default_chord().clone())
    }

    fn replace(&mut self, action: ShortcutAction, chord: Chord) -> Result<(), String> {
        action.accepts(&chord)?;
        if let Some(conflict) = ShortcutAction::ALL
            .into_iter()
            .find(|&other| other != action && *self.chord(other) == chord)
        {
            return Err(format!("此组合键已用于「{}」。", conflict.label()));
        }
        if chord == *action.default_chord() {
            self.0.remove(action.id());
        } else {
            self.0.insert(action.id().into(), chord);
        }
        Ok(())
    }

    pub fn normalize(&mut self) {
        self.0.retain(|id, chord| {
            chord.key.make_ascii_lowercase();
            chord.modifiers.function = false;
            ShortcutAction::ALL
                .into_iter()
                .find(|action| action.id() == id)
                .is_some_and(|action| {
                    action.accepts(chord).is_ok() && chord != action.default_chord()
                })
        });
        // Keep defaults and valid chains of overrides even when a manually edited file has collisions.
        while let Some(conflict) =
            ShortcutAction::ALL
                .into_iter()
                .enumerate()
                .find_map(|(index, action)| {
                    if !self.customized(action) {
                        return None;
                    }
                    let chord = self.chord(action);
                    ShortcutAction::ALL
                        .into_iter()
                        .enumerate()
                        .any(|(other_index, other)| {
                            other != action
                                && (!self.customized(other) || other_index < index)
                                && self.chord(other) == chord
                        })
                        .then_some(action)
                })
        {
            self.0.remove(conflict.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn application_key(key: &str) -> Keystroke {
        Keystroke {
            key: key.to_owned(),
            ..ShortcutBindings::default().keystroke(ShortcutAction::Palette)
        }
    }

    #[test]
    fn changes_dispatch_and_round_trips_through_settings() {
        let mut settings: crate::config::Settings = serde_json::from_str("{}").unwrap();
        let original = settings.shortcuts.keystroke(ShortcutAction::SearchTerminal);
        let updated = application_key("k");
        assert_eq!(
            settings.shortcuts.action(&original),
            Some(ShortcutAction::SearchTerminal)
        );
        settings
            .shortcuts
            .set(ShortcutAction::SearchTerminal, &updated)
            .unwrap();

        let encoded = serde_json::to_string(&settings).unwrap();
        let mut loaded: crate::config::Settings = serde_json::from_str(&encoded).unwrap();
        loaded.shortcuts.normalize();
        assert_eq!(loaded.shortcuts.action(&original), None);
        assert_eq!(
            loaded.shortcuts.action(&updated),
            Some(ShortcutAction::SearchTerminal)
        );
        loaded
            .shortcuts
            .reset(ShortcutAction::SearchTerminal)
            .unwrap();
        assert_eq!(
            loaded.shortcuts.action(&original),
            Some(ShortcutAction::SearchTerminal)
        );
        assert_eq!(loaded.shortcuts.action(&updated), None);
    }

    #[test]
    fn rejects_conflicts_and_reserved_input() {
        let mut bindings = ShortcutBindings::default();
        let action = ShortcutAction::Palette;
        let new_session = bindings.keystroke(ShortcutAction::NewSession);
        let plain_key = Keystroke::parse("p").unwrap();
        assert!(bindings.set(action, &new_session).is_err());
        assert!(bindings.set(action, &application_key("c")).is_err());
        assert!(bindings.set(action, &plain_key).is_err());
        assert!(!bindings.has_customizations());
    }

    #[test]
    fn preserves_valid_reuse_of_a_vacated_default() {
        let mut bindings = ShortcutBindings::default();
        let action = ShortcutAction::Palette;
        let previous = bindings.keystroke(action);
        bindings.set(action, &application_key("k")).unwrap();
        bindings.set(ShortcutAction::NewSession, &previous).unwrap();
        bindings.normalize();
        assert_eq!(bindings.action(&previous), Some(ShortcutAction::NewSession));
        assert!(bindings.reset(action).is_err());
        bindings.reset(ShortcutAction::NewSession).unwrap();
        bindings.reset(action).unwrap();
        assert_eq!(bindings.action(&previous), Some(action));
    }
}
