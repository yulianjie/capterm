use gpui_kit::assets::IconName;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, Theme};
use gpui_kit::prelude::*;
use std::fmt::Write as _;

use super::ui::{Palette, button, col, input, row};
use super::*;

type Invoke = Rc<dyn Fn(&mut Workbench, &mut Window, &mut Context<Workbench>)>;

#[derive(Clone)]
struct Choice {
    title: String,
    detail: String,
    icon: IconName,
    action: Invoke,
}

impl Choice {
    fn new(
        title: impl Into<String>,
        detail: impl Into<String>,
        icon: IconName,
        action: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            icon,
            action: Rc::new(action),
        }
    }

    fn matches(&self, words: &str) -> bool {
        let text = format!("{} {}", self.title, self.detail).to_lowercase();
        words.split_whitespace().all(|word| text.contains(word))
    }
}

struct Picker {
    title: &'static str,
    query: Entity<InputState>,
    entries: Vec<Choice>,
    owner: WeakEntity<Workbench>,
    index: usize,
    ssh: bool,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Picker {
    fn choices(&self, cx: &App) -> Vec<Choice> {
        let text = self.query.read(cx).value();
        let mut items = Vec::new();
        if self.ssh
            && let Some(profile) = quick_profile(&text)
        {
            items.push(Choice::new(
                format!("连接到 {}", ui::target(&profile)),
                "快速连接 · 不保存会话",
                IconName::Terminal,
                move |view, window, cx| view.connect(profile.clone(), None, window, cx),
            ));
        }
        let words = text.to_lowercase();
        items.extend(
            self.entries
                .iter()
                .filter(|item| item.matches(&words))
                .cloned(),
        );
        items
    }

    fn run(owner: &WeakEntity<Workbench>, choice: Choice, window: &mut Window, cx: &mut App) {
        window.close_dialog(cx);
        let _ = owner.update(cx, |view, cx| (choice.action)(view, window, cx));
    }
}

impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::new(!Theme::global(cx).is_dark());
        let items = self.choices(cx);
        let owner = self.owner.clone();
        col()
            .key_context("CapTermPicker")
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                let count = view.choices(cx).len();
                match event.keystroke.key.as_str() {
                    "up" | "down" if count > 0 => {
                        view.index = if event.keystroke.key == "up" {
                            view.index.checked_sub(1).unwrap_or(count - 1)
                        } else {
                            (view.index + 1) % count
                        };
                        view.scroll.scroll_to_item(view.index);
                        cx.notify();
                    }
                    "enter" if count > 0 && !event.keystroke.modifiers.modified() => {
                        if let Some(item) = view.choices(cx).get(view.index.min(count - 1)).cloned()
                        {
                            Self::run(&view.owner, item, window, cx);
                        }
                    }
                    _ => return,
                }
                window.prevent_default();
                cx.stop_propagation();
            }))
            .child(
                row()
                    .h(px(42.))
                    .px(px(12.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(Icon::new(IconName::Search).size(px(15.)).text_color(p.dim))
                    .child(
                        input(&self.query, self.title, p)
                            .appearance(false)
                            .bordered(false),
                    ),
            )
            .child(
                col()
                    .id("command-results")
                    .track_scroll(&self.scroll)
                    .max_h(px(336.).min(window.viewport_size().height / 2.))
                    .overflow_y_scroll()
                    .p(px(5.))
                    .when(items.is_empty(), |list| {
                        list.child(ui::muted_message("没有匹配的结果", p))
                    })
                    .children(items.into_iter().enumerate().map(|(index, choice)| {
                        let owner = owner.clone();
                        let clicked = choice.clone();
                        row()
                            .id(("command-choice", index))
                            .h(px(38.))
                            .flex_shrink_0()
                            .gap(px(9.))
                            .px(px(9.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .bg(if index == self.index {
                                p.tint
                            } else {
                                p.raised
                            })
                            .hover(|s| s.bg(p.hover))
                            .child(Icon::new(choice.icon).size(px(15.)).text_color(p.accent))
                            .child(div().flex_1().truncate().child(choice.title))
                            .child(
                                div()
                                    .max_w(relative(0.45))
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(p.dim)
                                    .child(choice.detail),
                            )
                            .on_click(move |_, window, cx| {
                                Self::run(&owner, clicked.clone(), window, cx)
                            })
                    })),
            )
            .child(
                row()
                    .h(px(29.))
                    .px(px(12.))
                    .border_t_1()
                    .border_color(p.line)
                    .text_color(p.dim)
                    .text_size(px(11.))
                    .child("↑ ↓ 选择     Enter 执行     Esc 关闭"),
            )
    }
}

fn quick_profile(value: &str) -> Option<ConnectionProfile> {
    let value = value.trim();
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (host, user, port) = config::parse_hop(value).ok()?;
    let user = user?;
    let port = port.unwrap_or(22);
    Some(ConnectionProfile {
        id: format!("quick:{user}@{host}:{port}"),
        name: host.to_owned(),
        host: host.to_owned(),
        user: user.to_owned(),
        port,
        identity_files: Vec::new(),
        jumps: Vec::new(),
        source: None,
        group: String::new(),
        ssh: config::SshOptions::default(),
    })
}

impl Workbench {
    pub(super) fn install_shortcuts(window: &Window, cx: &mut Context<Self>) -> Subscription {
        let weak = cx.weak_entity();
        let window_id = window.window_handle().window_id();
        cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() != window_id || window.has_active_dialog(cx) {
                return;
            }
            let handled = weak
                .update(cx, |view, cx| view.shortcut(&event.keystroke, window, cx))
                .unwrap_or(false);
            if handled {
                window.prevent_default();
                cx.stop_propagation();
            }
        })
    }

    fn shortcut(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = key.modifiers;
        let action = if cfg!(target_os = "macos") {
            m.platform && !m.control && !m.shift
        } else {
            m.control && m.shift && !m.platform
        };
        let control = if cfg!(target_os = "macos") {
            m.platform
        } else {
            m.control
        };
        if !m.alt && action {
            match key.key.as_str() {
                "p" => self.command_palette(window, cx),
                "n" => self.profile_dialog(None, window, cx),
                "t" => self.duplicate_active(window, cx),
                "w" => self.close_active(window, cx),
                "tab" if !cfg!(target_os = "macos") => self.cycle_tabs(true, window, cx),
                "f" if self.active_file_editor.is_none() => self.search_terminal(window, cx),
                "i" => self.focus_sender(window, cx),
                "g" => self.toggle_gutters(None, cx),
                "e" => self.toggle_left(cx),
                "b" => self.toggle_right(cx),
                "l" => self.toggle_theme(window, cx),
                "enter" => self.toggle_focus_mode(cx),
                "c" | "v" if cfg!(target_os = "macos") => return false,
                _ => return false,
            }
            return true;
        }
        if !m.alt && control {
            match key.key.as_str() {
                "tab" => self.cycle_tabs(m.shift, window, cx),
                "," if !m.shift => self.settings_dialog(window, cx),
                "s" if !m.shift && self.active_file_editor.is_some() => {
                    self.save_active_file_editor(window, cx)
                }
                "r" if !m.shift && self.sender_focused(window, cx) => {
                    self.history_picker(window, cx)
                }
                _ => return false,
            }
            return true;
        }
        if !m.modified()
            && self.sender_focused(window, cx)
            && matches!(key.key.as_str(), "up" | "down")
        {
            return self.navigate_history(key.key == "up", window, cx);
        }
        false
    }

    fn picker(
        &self,
        title: &'static str,
        placeholder: &'static str,
        entries: Vec<Choice>,
        ssh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let owner = cx.weak_entity();
        let picker = cx.new(|cx| {
            let sub = cx.subscribe(&query, |picker: &mut Picker, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    picker.index = 0;
                    picker.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
            });
            Picker {
                title,
                query: query.clone(),
                entries,
                owner,
                index: 0,
                ssh,
                scroll: ScrollHandle::new(),
                _subscriptions: vec![sub],
            }
        });
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                query.focus_handle(cx).focus(window, cx);
            }
            dialog
                .w(px(620.).min(window.viewport_size().width - px(30.)))
                .margin_top(px(55.))
                .p_0()
                .overflow_hidden()
                .rounded(px(10.))
                .child(picker.clone())
        });
    }

    pub(super) fn command_palette(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = self
            .settings
            .profiles
            .iter()
            .map(|profile| {
                let saved = profile.clone();
                Choice::new(
                    profile.name.clone(),
                    ui::target(profile),
                    IconName::Server,
                    move |view, window, cx| view.connect(saved.clone(), None, window, cx),
                )
            })
            .collect::<Vec<_>>();
        items.extend([
            Choice::new("新建会话", "Ctrl+Shift+N", IconName::Plus, |v, w, c| {
                v.profile_dialog(None, w, c)
            }),
            Choice::new(
                "导入 SSH 配置",
                "",
                IconName::Import,
                Workbench::import_dialog,
            ),
            Choice::new(
                "设置",
                "Ctrl+,",
                IconName::Settings,
                Workbench::settings_dialog,
            ),
            Choice::new(
                "搜索终端",
                "Ctrl+Shift+F",
                IconName::Search,
                Workbench::search_terminal,
            ),
            Choice::new(
                "切换主题",
                "Ctrl+Shift+L",
                IconName::Sun,
                Workbench::toggle_theme,
            ),
            Choice::new(
                "专注模式",
                "Ctrl+Shift+Enter",
                IconName::Maximize,
                |v, _, c| v.toggle_focus_mode(c),
            ),
            Choice::new(
                "命令历史",
                "Ctrl+R（输入栏）",
                IconName::Clock,
                |v, w, c| v.history_picker(w, c),
            ),
            Choice::new("命令片段", "", IconName::ScrollText, |v, w, c| {
                v.snippet_picker(w, c)
            }),
        ]);
        if cfg!(windows) {
            for shell in [
                LocalShell::PowerShell,
                LocalShell::CommandPrompt,
                LocalShell::Wsl,
            ] {
                items.push(Choice::new(
                    shell.label(),
                    "本地终端",
                    IconName::Terminal,
                    move |v, w, c| v.launch_local(shell, w, c),
                ));
            }
        }
        self.picker(
            "命令面板",
            "搜索会话、命令，或输入 user@host 快速连接",
            items,
            true,
            window,
            cx,
        );
    }

    pub(super) fn toggle_focus_mode(&mut self, cx: &mut Context<Self>) {
        self.focus_mode = !self.focus_mode;
        self.drawer = None;
        self.notifications_open = false;
        cx.notify();
    }

    pub(super) fn toggle_left(&mut self, cx: &mut Context<Self>) {
        if self.focus_mode {
            self.focus_mode = false;
        }
        if self.compact {
            self.drawer = (self.drawer != Some(Drawer::Files)).then_some(Drawer::Files);
        } else {
            self.left_visible = !self.left_visible;
            if self.drawer == Some(Drawer::Files) {
                self.drawer = None;
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_right(&mut self, cx: &mut Context<Self>) {
        if self.focus_mode {
            self.focus_mode = false;
        }
        self.right_visible = !self.right_visible;
        if matches!(self.drawer, Some(Drawer::Sessions | Drawer::Monitor)) {
            self.drawer = None;
        }
        cx.notify();
    }

    pub(super) fn toggle_gutters(&mut self, field: Option<bool>, cx: &mut Context<Self>) {
        let Some(tab) = self.active.and_then(|id| self.tab_mut(id)) else {
            return;
        };
        match field {
            Some(true) => tab.line_numbers = !tab.line_numbers,
            Some(false) => tab.timestamps = !tab.timestamps,
            None => {
                let show = !(tab.line_numbers || tab.timestamps);
                tab.line_numbers = show;
                tab.timestamps = show;
            }
        }
        tab.terminal.update(cx, |term, cx| {
            term.set_gutters(tab.line_numbers, tab.timestamps, cx)
        });
        cx.notify();
    }

    pub(super) fn search_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_file_editor.is_none()
            && let Some(tab) = self.active_tab()
        {
            tab.terminal
                .update(cx, |term, cx| term.open_search(window, cx));
        }
    }

    fn sender_focused(&self, window: &Window, cx: &App) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.draft.focus_handle(cx).is_focused(window))
    }

    pub(super) fn focus_sender(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active.and_then(|id| self.tab_mut(id)) {
            tab.show_sender = true;
            tab.draft.focus_handle(cx).focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn draft_receivers(&self, active: u64) -> Vec<u64> {
        self.tabs
            .iter()
            .filter(|tab| {
                tab.phase == Phase::Connected
                    && match &self.send_target {
                        SendTarget::Active => tab.id == active,
                        SendTarget::All => true,
                        SendTarget::Selected(ids) => ids.contains(&tab.id),
                    }
            })
            .map(|tab| tab.id)
            .collect()
    }

    pub(super) fn choose_targets(&self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = match &self.send_target {
            SendTarget::Selected(ids) => ids.clone(),
            _ => HashSet::new(),
        };
        let ids = Rc::new(RefCell::new(ids));
        let tabs = self
            .tabs
            .iter()
            .filter(|tab| tab.phase == Phase::Connected)
            .map(|tab| (tab.id, tab.name().to_owned()))
            .collect::<Vec<_>>();
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let p = Palette::new(!Theme::global(cx).is_dark());
            let done = ids.clone();
            let weak = weak.clone();
            let list = col().gap(px(6.)).children(tabs.iter().map(|(id, name)| {
                let id = *id;
                let ids = ids.clone();
                let checked = ids.borrow().contains(&id);
                gpui_kit::component::checkbox::Checkbox::new(("sender-receiver", id))
                    .label(name.clone())
                    .checked(checked)
                    .on_click(move |checked, window, _| {
                        if *checked {
                            ids.borrow_mut().insert(id);
                        } else {
                            ids.borrow_mut().remove(&id);
                        }
                        window.refresh();
                    })
            }));
            dialog.title("选择发送目标").child(list).footer(
                ui::primary("apply-receivers", "应用选择", p).on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |view, cx| {
                        view.send_target = SendTarget::Selected(done.borrow().clone());
                        cx.notify();
                    });
                    window.close_dialog(cx);
                }),
            )
        });
    }

    fn set_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_sender(window, cx);
        if let Some(tab) = self.active_tab() {
            tab.draft
                .update(cx, |draft, cx| draft.set_value(text.to_owned(), window, cx));
        }
    }

    fn navigate_history(
        &mut self,
        older: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.active.and_then(|id| self.tab_mut(id)) else {
            return false;
        };
        let history = tab.command_history.borrow();
        if history.is_empty() {
            return false;
        }
        let value = tab.draft.read(cx).value();
        if tab.history_index.is_none() {
            if !older || value.contains('\n') {
                return false;
            }
            tab.history_draft = value.to_string();
        }
        tab.history_index = if older {
            Some(
                tab.history_index
                    .map_or(0, |index| (index + 1).min(history.len() - 1)),
            )
        } else {
            tab.history_index.and_then(|index| index.checked_sub(1))
        };
        let text = tab.history_index.map_or_else(
            || tab.history_draft.clone(),
            |i| history[i.min(history.len() - 1)].clone(),
        );
        tab.draft
            .update(cx, |draft, cx| draft.set_value(text, window, cx));
        true
    }

    pub(super) fn history_picker(&self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.active_tab().map_or_else(Vec::new, |tab| {
            tab.command_history
                .borrow()
                .iter()
                .map(|command| {
                    let command = command.clone();
                    Choice::new(
                        command.clone(),
                        "命令历史",
                        IconName::Clock,
                        move |view, window, cx| view.set_draft(&command, window, cx),
                    )
                })
                .collect()
        });
        self.picker("命令历史", "搜索此会话的命令历史", items, false, window, cx);
    }

    pub(super) fn snippet_picker(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = Vec::new();
        if let Some(tab) = self.active_tab() {
            let current = tab.draft.read(cx).value().to_string();
            if !current.trim().is_empty() && !self.settings.snippets.contains(&current) {
                items.push(Choice::new(
                    "将当前输入保存为命令片段",
                    "",
                    IconName::Plus,
                    move |view, _, cx| {
                        let mut settings = view.settings.clone();
                        settings.snippets.push(current.clone());
                        if view.persist(settings, cx) {
                            view.announce("已保存命令片段", cx);
                        }
                    },
                ));
            }
        }
        for snippet in &self.settings.snippets {
            let snippet = snippet.clone();
            items.push(Choice::new(
                snippet.clone(),
                "插入片段",
                IconName::ScrollText,
                move |view, window, cx| view.fill_snippet(&snippet, window, cx),
            ));
        }
        self.picker("命令片段", "搜索命令片段", items, false, window, cx);
    }

    fn fill_snippet(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut names = Vec::<String>::new();
        let mut remaining = text;
        while let Some((_, token)) = remaining.split_once("{{") {
            let Some((name, rest)) = token.split_once("}}") else {
                break;
            };
            let name = name.trim().to_owned();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
            remaining = rest;
        }
        if names.is_empty() {
            self.set_draft(text, window, cx);
            return;
        }
        let values = names
            .into_iter()
            .map(|name| {
                let state = cx.new(|cx| InputState::new(window, cx).placeholder(name.clone()));
                (name, state)
            })
            .collect::<Vec<_>>();
        let weak = cx.weak_entity();
        let text = text.to_owned();
        window.open_dialog(cx, move |dialog, _, cx| {
            let p = Palette::new(!Theme::global(cx).is_dark());
            let weak = weak.clone();
            let fields = values.clone();
            let text = text.clone();
            dialog
                .title("填写片段变量")
                .child(
                    col()
                        .max_h(px(300.))
                        .id("snippet-fields")
                        .overflow_y_scroll()
                        .gap(px(9.))
                        .children(values.iter().map(|(name, field)| {
                            col()
                                .gap(px(3.))
                                .child(name.clone())
                                .child(input(field, "片段变量", p))
                        })),
                )
                .footer(
                    button("insert-snippet", "插入命令", p).on_click(move |_, window, cx| {
                        let mut result = String::new();
                        let mut remaining = text.as_str();
                        while let Some((prefix, token)) = remaining.split_once("{{") {
                            let Some((name, rest)) = token.split_once("}}") else {
                                break;
                            };
                            result.push_str(prefix);
                            if let Some((_, field)) =
                                fields.iter().find(|(key, _)| key == name.trim())
                            {
                                result.push_str(&field.read(cx).value());
                            } else {
                                result.push_str(&format!("{{{{{name}}}}}"));
                            }
                            remaining = rest;
                        }
                        result.push_str(remaining);
                        window.close_dialog(cx);
                        let _ = weak.update(cx, |view, cx| view.set_draft(&result, window, cx));
                    }),
                )
        });
    }

    pub(super) fn duplicate_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_tab().map(|tab| &tab.session) {
            Some(Session::Ssh(profile)) => self.connect_new(profile.clone(), None, window, cx),
            Some(Session::Local(shell)) => self.launch_local(*shell, window, cx),
            None => self.command_palette(window, cx),
        }
    }

    pub(super) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_file_editor {
            self.close_file_editor(editor, window, cx);
        } else if let Some(id) = self.active {
            self.close_tab(id, window, cx);
        }
    }

    fn cycle_tabs(&mut self, previous: bool, window: &mut Window, cx: &mut Context<Self>) {
        let terminals = self.tabs.len();
        let count = terminals + self.file_editors.len();
        if count == 0 {
            return;
        }
        let index = if let Some(active) = self.active_file_editor {
            self.file_editors
                .iter()
                .position(|tab| tab.id == active)
                .map(|index| terminals + index)
        } else {
            self.tabs.iter().position(|tab| Some(tab.id) == self.active)
        };
        let index = match (previous, index) {
            (true, Some(i)) => (i + count - 1) % count,
            (false, Some(i)) => (i + 1) % count,
            (true, None) => count - 1,
            (false, None) => 0,
        };
        if index < terminals {
            self.select_tab(self.tabs[index].id, window, cx);
        } else {
            self.activate_file_editor(self.file_editors[index - terminals].id, window, cx);
        }
    }

    pub(super) fn observe_terminal(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id) else { return };
        let terminal = tab.terminal.read(cx);
        let observed = &mut tab.observed_terminal;
        let changed_dir = terminal.cwd() != observed.directory.as_deref();
        if changed_dir {
            observed.directory = terminal.cwd().map(str::to_owned);
        }
        let dir = changed_dir.then(|| observed.directory.clone()).flatten();
        let prompt = observed.prompt != terminal.prompt_serial();
        let note = (observed.notification != terminal.notification_serial())
            .then(|| terminal.notification().unwrap_or("终端响铃").to_owned());
        if !observed.fullscreen && terminal.raw() {
            tab.show_sender = false;
        }
        observed.fullscreen = terminal.raw();
        observed.prompt = terminal.prompt_serial();
        observed.notification = terminal.notification_serial();
        if let Some(dir) = dir {
            self.file_terminal_directory(id, dir, cx);
        }
        if prompt {
            self.file_prompt_ready(id, cx);
        }
        if let Some(note) = note
            && (self.active != Some(id)
                || !window.is_window_active()
                || self.active_file_editor.is_some())
        {
            self.terminal_notice(id, note, window, cx);
        }
        cx.notify();
    }

    fn terminal_notice(
        &mut self,
        id: u64,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id) else { return };
        tab.pending_notification = true;
        let label = tab.name().to_owned();
        let message = message
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(512)
            .collect::<String>();
        self.notifications.push_front(SessionNotice {
            tab: id,
            label: label.clone(),
            message: message.clone(),
            time: chrono::Local::now(),
            read: false,
        });
        self.notifications.truncate(50);
        let weak = cx.weak_entity();
        let notification = Notification::new()
            .id1::<SessionNotice>(("terminal-note", id))
            .title(format!("{label}：等待确认"))
            .message(message)
            .on_click(move |_, window, cx| {
                let _ = weak.update(cx, |view, cx| view.select_tab(id, window, cx));
            });
        let notification = if window.is_window_active() {
            notification
        } else {
            notification.in_app_and_system()
        };
        window.push_notification(notification, cx);
        cx.notify();
    }

    pub(super) fn export_terminal(&self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else { return };
        let log = tab.terminal.read(cx).formatted_log();
        self.export_text(log, "capterm-session.log", "会话日志", window, cx);
    }

    pub(super) fn export_ssh_config(
        &mut self,
        profile: &ConnectionProfile,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let text = match ssh_config_fragment(profile) {
            Ok(text) => text,
            Err(error) => {
                self.announce(format!("无法导出 SSH 配置：{error}"), cx);
                return;
            }
        };
        self.export_text(text, "capterm-ssh.conf", "SSH 配置片段", window, cx);
    }

    fn export_text(
        &self,
        contents: String,
        filename: &'static str,
        label: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let path =
            cx.prompt_for_new_path(&dirs::document_dir().unwrap_or_default(), Some(filename));
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, contents).map(|()| path) })
                .await;
            let _ = weak.update_in(cx, |view, _, cx| {
                view.announce(
                    match result {
                        Ok(path) => format!("已导出{label}：{}", path.display()),
                        Err(error) => format!("导出失败：{error}"),
                    },
                    cx,
                );
            });
        })
        .detach();
    }
}

fn ssh_config_fragment(profile: &ConnectionProfile) -> anyhow::Result<String> {
    let alias = profile
        .name
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let alias = alias.trim_matches(['-', '.']);
    let alias = if alias.is_empty() {
        "capterm-session"
    } else {
        alias
    };
    let mut text = String::from("# CapTerm SSH config\n");
    let mut jumps = Vec::new();
    for (index, hop) in profile.jumps.iter().enumerate() {
        let jump = format!("{alias}-capterm-jump-{}", index + 1);
        write_host(
            &mut text,
            &jump,
            &hop.host,
            &hop.user,
            hop.port,
            &hop.identity_files,
            &hop.ssh,
        )?;
        text.push('\n');
        jumps.push(jump);
    }
    write_host(
        &mut text,
        alias,
        &profile.host,
        &profile.user,
        profile.port,
        &profile.identity_files,
        &profile.ssh,
    )?;
    if !jumps.is_empty() {
        writeln!(text, "    ProxyJump {}", jumps.join(","))?;
    }
    for forward in &profile.ssh.port_forwards {
        writeln!(text, "    {forward}")?;
    }
    Ok(text)
}

fn write_host(
    out: &mut String,
    alias: &str,
    host: &str,
    user: &str,
    port: u16,
    identities: &[String],
    ssh: &config::SshOptions,
) -> anyhow::Result<()> {
    writeln!(out, "Host {alias}")?;
    for (name, value) in [("HostName", host), ("User", user)] {
        write_ssh_value(out, name, value)?;
    }
    writeln!(out, "    Port {port}")?;
    for path in identities {
        write_ssh_value(out, "IdentityFile", path)?;
    }
    if ssh.identities_only {
        writeln!(out, "    IdentitiesOnly yes")?;
    }
    if ssh.forward_agent {
        writeln!(out, "    ForwardAgent yes")?;
    }
    if ssh.compression {
        writeln!(out, "    Compression yes")?;
    }
    if let Some(agent) = &ssh.identity_agent {
        write_ssh_value(out, "IdentityAgent", agent)?;
    }
    if let Some(timeout) = ssh.connect_timeout {
        writeln!(out, "    ConnectTimeout {timeout}")?;
    }
    if let Some(interval) = ssh.keepalive_interval {
        writeln!(out, "    ServerAliveInterval {interval}")?;
    }
    for (key, value) in &ssh.set_env {
        write_ssh_value(out, "SetEnv", &format!("{key}={value}"))?;
    }
    for pattern in &ssh.send_env {
        write_ssh_value(out, "SendEnv", pattern)?;
    }
    if let Some(proxy) = &ssh.network_proxy {
        writeln!(
            out,
            "    # CapTerm TCP proxy {proxy}. Set an equivalent ProxyCommand for OpenSSH."
        )?;
    }
    if let Some(args) = &ssh.proxy_command {
        anyhow::ensure!(
            [host, user]
                .into_iter()
                .all(|value| value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "-._:".contains(character)
                })),
            "ProxyCommand 的目标地址或用户名含有无法安全展开的字符"
        );
        let mut shell_args = Vec::with_capacity(args.len());
        for arg in args {
            anyhow::ensure!(!arg.chars().any(char::is_control), "代理命令包含控制字符");
            shell_args.push(format!("'{}'", arg.replace('\'', "'\\''")));
        }
        writeln!(out, "    # ProxyCommand below uses POSIX shell quoting.")?;
        writeln!(out, "    ProxyCommand {}", shell_args.join(" "))?;
    }
    Ok(())
}

fn write_ssh_value(out: &mut String, name: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!value.chars().any(char::is_control), "{name} 包含控制字符");
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    writeln!(out, "    {name} \"{escaped}\"")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{config, quick_profile, ssh_config_fragment};

    #[test]
    fn ssh_config_fragment_preserves_jump_credentials() -> anyhow::Result<()> {
        let mut profile = quick_profile("deploy@target.example:2222").unwrap();
        profile.name = "My QA * Session".into();
        let mut jump = quick_profile("ops@jump.example").unwrap();
        let directory = tempfile::tempdir()?;
        let key = directory.path().join("jump key");
        jump.identity_files = vec![key.to_string_lossy().into_owned()];
        profile.jumps.push(config::Hop {
            host: jump.host,
            user: jump.user,
            port: jump.port,
            identity_files: jump.identity_files,
            ssh: jump.ssh,
            reference: None,
        });
        let path = directory.path().join("export.conf");
        std::fs::write(&path, ssh_config_fragment(&profile)?)?;
        let imported = config::import(&path.to_string_lossy())?;
        let target = imported
            .profiles
            .iter()
            .find(|profile| profile.host == "target.example")
            .expect("exported target imports");
        assert_eq!(target.port, 2222);
        assert_eq!(target.jumps[0].user, "ops");
        assert_eq!(
            std::path::Path::new(&target.jumps[0].identity_files[0]),
            key
        );
        Ok(())
    }
}
