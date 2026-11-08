mod commands;
mod dialogs;
mod files;
mod monitor;
mod ui;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::Write;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::{Theme, TitleBar, WindowExt};
use gpui_kit::*;
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;

use crate::config::{self, ConnectionProfile, Settings, ThemePreference};
use crate::local::{self, LocalShell};
use crate::ssh::{
    Connection, ConnectionPrompt, HopProgress, HopStage, Metrics, ShellCommand, ShellEvent,
};
use crate::terminal::highlight::{BUNDLED, LinuxHighlights};
use crate::terminal::{ClipboardImage, TerminalCommand, TerminalSize, TerminalView};
use files::{FileEvent, FilePane};

const DRAFT_HINT: &str = "输入命令，Enter 发送，↑ 调出历史";
const SENDER_HISTORY_LIMIT: usize = 200;
const SENDER_HISTORY_ENTRY_BYTES: usize = 16 * 1024;

pub fn open(cx: &mut App) {
    let (settings, load_error) = match Settings::load() {
        Ok(settings) => (settings, None),
        Err(error) => (
            Settings::default(),
            Some(format!("无法读取设置：{error:#}")),
        ),
    };
    ui::install_theme(cx);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        // The session runtime primarily waits on I/O. GPUI has its own workers.
        .worker_threads(std::thread::available_parallelism().map_or(2, |count| count.get().min(4)))
        .thread_name("capterm-session")
        .enable_all()
        .build()
        .expect("start session runtime");
    let window_bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    let mut options = TitleBar::window_options();
    options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    options.window_min_size = Some(size(px(500.), px(460.)));
    if let Some(titlebar) = &mut options.titlebar {
        titlebar.title = Some("CapTerm".into());
    }
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workbench::new(settings, load_error, runtime, window, cx))
    })
    .expect("open CapTerm workbench");
    cx.activate(true);
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Connecting,
    Connected,
    Disconnected,
    Failed,
}

impl Phase {
    fn label(self, local: bool) -> &'static str {
        match (self, local) {
            (Self::Connecting, true) => "正在启动",
            (Self::Connected, true) => "运行中",
            (Self::Disconnected, true) => "已退出",
            (Self::Failed, true) => "启动失败",
            (Self::Connecting, false) => "正在连接",
            (Self::Connected, false) => "已连接",
            (Self::Disconnected, false) => "未连接",
            (Self::Failed, false) => "连接失败",
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Drawer {
    Files,
    Sessions,
    Monitor,
}

#[derive(Clone, Default)]
enum SendTarget {
    #[default]
    Active,
    All,
    Selected(HashSet<u64>),
}

struct SessionNotice {
    tab: u64,
    label: String,
    message: String,
    time: chrono::DateTime<chrono::Local>,
    read: bool,
}

#[derive(Default)]
struct TerminalObservation {
    directory: Option<String>,
    prompt: u64,
    notification: u64,
    fullscreen: bool,
    agent_running: bool,
}

struct ConnectionTest {
    profile: ConnectionProfile,
    started: Instant,
    hops: Vec<HopProgress>,
    form: Option<Rc<dialogs::FormStatus>>,
    _task: AbortHandle,
}

#[derive(Default)]
struct TerminalLink {
    size: Cell<TerminalSize>,
    commands: RefCell<Option<ShellInput>>,
}

enum ShellInput {
    Ssh(mpsc::UnboundedSender<ShellCommand>),
    Local(std::sync::mpsc::Sender<ShellCommand>),
}

impl TerminalLink {
    fn forward(&self, command: TerminalCommand) {
        let command = match command {
            TerminalCommand::Data(bytes) => ShellCommand::Input(bytes),
            TerminalCommand::Resize(size) => {
                self.size.set(size);
                ShellCommand::Resize {
                    columns: size.cols,
                    rows: size.rows,
                }
            }
        };
        match self.commands.borrow().as_ref() {
            Some(ShellInput::Ssh(commands)) => {
                let _ = commands.send(command);
            }
            Some(ShellInput::Local(commands)) => {
                let _ = commands.send(command);
            }
            None => {}
        }
    }
}

enum Session {
    Ssh(ConnectionProfile),
    Local(LocalShell),
}

struct Tab {
    id: u64,
    operation_id: u64,
    session: Session,
    ssh_password_auth: bool,
    phase: Phase,
    terminal: Entity<TerminalView>,
    draft: Entity<TextareaState>,
    link: Rc<TerminalLink>,
    line_numbers: bool,
    timestamps: bool,
    show_sender: bool,
    paste_mode: Option<bool>,
    command_history: Rc<RefCell<VecDeque<String>>>,
    history_index: Option<usize>,
    history_draft: String,
    pending_notification: bool,
    observed_terminal: TerminalObservation,
    route: Vec<HopProgress>,
    connection: Option<Arc<Connection>>,
    operation: Option<tokio::task::JoinHandle<()>>,
    monitor: Option<AbortHandle>,
    monitor_state: monitor::MonitorState,
    sample: Option<Arc<Metrics>>,
    files: FilePane,
    _draft_subscription: Subscription,
    _terminal_subscriptions: [Subscription; 2],
}

impl Tab {
    fn name(&self) -> &str {
        match &self.session {
            Session::Ssh(profile) => &profile.name,
            Session::Local(shell) => shell.label(),
        }
    }

    fn ssh_profile(&self) -> Option<&ConnectionProfile> {
        match &self.session {
            Session::Ssh(profile) => Some(profile),
            Session::Local(_) => None,
        }
    }

    fn can_reconnect(&self) -> bool {
        !self.is_local() && matches!(self.phase, Phase::Disconnected | Phase::Failed)
    }

    fn is_local(&self) -> bool {
        matches!(self.session, Session::Local(_))
    }

    fn has_file_browser(&self) -> bool {
        self.phase == Phase::Connected && (self.is_local() || self.connection.is_some())
    }

    fn bind_shell(&mut self, commands: ShellInput) {
        self.phase = Phase::Connected;
        let size = self.link.size.get();
        *self.link.commands.borrow_mut() = Some(commands);
        self.link.forward(TerminalCommand::Resize(size));
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.link.commands.borrow_mut().take();
        if let Some(operation) = self.operation.take()
            && matches!(self.phase, Phase::Connecting | Phase::Connected)
        {
            operation.abort();
        }
        if let Some(monitor) = self.monitor.take() {
            monitor.abort();
        }
    }
}

enum Event {
    Trust(u64, ConnectionPrompt),
    Connected(u64, Arc<Connection>, mpsc::UnboundedSender<ShellCommand>),
    LocalStarted(
        u64,
        std::sync::mpsc::Sender<ShellCommand>,
        Result<Option<local::WslPaths>, String>,
    ),
    StartFailed(u64, String),
    TestFinished(u64, Result<(), String>),
    Route(u64, HopProgress),
    Shell(u64, ShellEvent),
    Sample(u64, Metrics),
    File(u64, FileEvent),
    DragFinished(String),
}

struct Workbench {
    settings: Settings,
    linux_highlights: Rc<LinuxHighlights>,
    load_error: Option<String>,
    events: async_channel::Sender<Event>,
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
    filter: Entity<InputState>,
    empty_draft: Entity<TextareaState>,
    send_target: SendTarget,
    sender_histories: HashMap<String, Rc<RefCell<VecDeque<String>>>>,
    left_visible: bool,
    right_visible: bool,
    focus_mode: bool,
    left_width: Pixels,
    right_width: Pixels,
    monitor_height: Pixels,
    collapsed_groups: HashSet<String>,
    drawer: Option<Drawer>,
    compact: bool,
    notice: String,
    notifications: VecDeque<SessionNotice>,
    notifications_open: bool,
    tests: HashMap<u64, ConnectionTest>,
    recent_tests: VecDeque<(String, Vec<HopProgress>)>,
    window_active: bool,
    monitored_operation: watch::Sender<Option<u64>>,
    monitor_interval: Arc<AtomicU64>,
    file_editors: Vec<files::FileEditor>,
    active_file_editor: Option<u64>,
    _pump: Task<()>,
    _subscriptions: Vec<Subscription>,
    runtime: tokio::runtime::Runtime,
}

impl Workbench {
    fn new(
        mut settings: Settings,
        load_error: Option<String>,
        runtime: tokio::runtime::Runtime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (events, receiver) = async_channel::bounded(256);
        let pump = cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = receiver.recv().await {
                if view
                    .update_in(cx, |this, window, cx| this.apply_event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        if settings.theme == ThemePreference::System {
            Theme::sync_system_appearance(Some(window), cx);
            settings.light = !Theme::global(cx).is_dark();
        }
        ui::apply_theme(&settings, cx);
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称、主机或用户"));
        let filter_subscription = cx.subscribe(&filter, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let activation = cx.observe_window_activation(window, |this, window, _| {
            this.window_active = window.is_window_active();
            this.set_monitored_tab(this.active);
        });
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.settings.theme == ThemePreference::System {
                Theme::sync_system_appearance(Some(window), cx);
                this.update_theme(!Theme::global(cx).is_dark(), cx);
            }
        });
        let keys = Self::install_shortcuts(window, cx);
        let monitor_interval = Arc::new(AtomicU64::new(settings.monitor_interval.clamp(1, 60)));
        let (linux_highlights, highlight_error) =
            match config::linux_highlights_path().and_then(|path| LinuxHighlights::load(&path)) {
                Ok(rules) => (rules, None),
                Err(error) => (
                    LinuxHighlights::bundled(),
                    Some(format!(
                        "无法读取 Linux 高亮规则，已使用内置规则：{error:#}"
                    )),
                ),
            };
        let mut this = Self {
            notice: load_error.clone().or(highlight_error).unwrap_or_default(),
            settings,
            linux_highlights: Rc::new(linux_highlights),
            load_error,
            events,
            tabs: Vec::new(),
            active: None,
            next_id: 0,
            filter,
            empty_draft: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 6)
                    .placeholder(DRAFT_HINT)
            }),
            send_target: SendTarget::default(),
            sender_histories: HashMap::new(),
            left_visible: true,
            right_visible: true,
            focus_mode: false,
            left_width: px(272.),
            right_width: px(296.),
            monitor_height: px(424.),
            collapsed_groups: HashSet::new(),
            drawer: None,
            compact: window.viewport_size().width < px(900.),
            notifications: VecDeque::new(),
            notifications_open: false,
            tests: HashMap::new(),
            recent_tests: VecDeque::new(),
            window_active: window.is_window_active(),
            monitored_operation: watch::Sender::new(None),
            monitor_interval,
            file_editors: Vec::new(),
            active_file_editor: None,
            _pump: pump,
            _subscriptions: vec![filter_subscription, activation, appearance, keys],
            runtime,
        };
        this.start_linked_config_sync(window, cx);
        this
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    fn tab(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn tab_mut(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn tab_for_operation(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.operation_id == id)
    }

    fn tab_mut_for_operation(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.operation_id == id)
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn announce(&mut self, notice: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = notice.into();
        cx.notify();
    }

    fn persist(&mut self, mut settings: Settings, cx: &mut Context<Self>) -> bool {
        settings
            .sender_history
            .retain(|id, _| settings.profiles.iter().any(|profile| &profile.id == id));
        let result = match &self.load_error {
            Some(error) => Err(anyhow::anyhow!("{error}")),
            None => settings.save(),
        };
        match result {
            Ok(()) => {
                self.settings = settings;
                self.monitor_interval.store(
                    self.settings.monitor_interval.clamp(1, 60),
                    Ordering::Relaxed,
                );
                cx.notify();
                true
            }
            Err(error) => {
                self.announce(format!("无法保存设置：{error:#}"), cx);
                false
            }
        }
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = if self.settings.light {
            ThemePreference::Dark
        } else {
            ThemePreference::Light
        };
        self.set_theme_preference(theme, window, cx);
    }

    fn set_theme_preference(
        &mut self,
        theme: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.settings.clone();
        settings.theme = theme;
        settings.light = match theme {
            ThemePreference::Legacy => settings.light,
            ThemePreference::Dark => false,
            ThemePreference::Light => true,
            ThemePreference::System => {
                !gpui_kit::component::ThemeMode::from(window.appearance()).is_dark()
            }
        };
        let light = settings.light;
        if self.persist(settings, cx) {
            self.update_theme(light, cx);
        }
    }

    fn update_theme(&mut self, light: bool, cx: &mut Context<Self>) {
        self.settings.light = light;
        ui::apply_theme(&self.settings, cx);
        for tab in &self.tabs {
            tab.terminal
                .update(cx, |terminal, cx| terminal.set_light(light, cx));
        }
        cx.notify();
    }

    fn apply_terminal_preferences(&mut self, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            tab.line_numbers = self.settings.show_line_numbers;
            tab.timestamps = self.settings.show_timestamps;
            tab.terminal.update(cx, |term, cx| {
                configure_terminal(
                    term,
                    &self.settings,
                    &self.linux_highlights,
                    &tab.session,
                    cx,
                );
            });
        }
        cx.notify();
    }

    fn edit_linux_highlights(&mut self, cx: &mut Context<Self>) {
        let result = (|| -> anyhow::Result<_> {
            let path = config::linux_highlights_path()?;
            if !path.try_exists()? {
                let directory = path.parent().expect("Linux highlights settings directory");
                std::fs::create_dir_all(directory)?;
                let mut file = tempfile::NamedTempFile::new_in(directory)?;
                file.write_all(BUNDLED.as_bytes())?;
                if let Err(error) = file.persist_noclobber(&path)
                    && error.error.kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(error.into());
                }
            }
            Ok(path)
        })();
        match result {
            Ok(path) => cx.open_with_system(&path),
            Err(error) => self.announce(format!("无法打开 Linux 高亮规则：{error:#}"), cx),
        }
    }

    fn reload_linux_highlights(&mut self, cx: &mut Context<Self>) {
        match config::linux_highlights_path().and_then(|path| LinuxHighlights::load(&path)) {
            Ok(rules) => {
                self.linux_highlights = Rc::new(rules);
                self.apply_terminal_preferences(cx);
                self.announce("已重载 Linux 高亮规则", cx);
            }
            Err(error) => self.announce(format!("Linux 高亮规则未更新：{error:#}"), cx),
        }
    }

    fn apply_font_preferences(&mut self, cx: &mut Context<Self>) {
        ui::apply_theme(&self.settings, cx);
        self.apply_terminal_preferences(cx);
    }

    fn select_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab(id) {
            tab.terminal
                .update(cx, |terminal, cx| terminal.focus(window, cx));
            self.active = Some(id);
            self.active_file_editor = None;
            self.set_monitored_tab(Some(id));
            if let Some(tab) = self.tab_mut(id) {
                tab.pending_notification = false;
            }
            for note in self.notifications.iter_mut().filter(|note| note.tab == id) {
                note.read = true;
            }
            self.drawer = None;
            cx.notify();
        }
    }

    fn set_monitored_tab(&self, id: Option<u64>) {
        let monitored = id
            .and_then(|id| self.tab(id))
            .filter(|tab| self.window_active && !tab.is_local() && !tab.monitor_state.paused)
            .map(|tab| tab.operation_id);
        self.monitored_operation.send_if_modified(|current| {
            if *current == monitored {
                return false;
            }
            *current = monitored;
            true
        });
    }

    fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.cancel_file_transfers(id, cx);
        let mut tab = self.tabs.remove(index);
        if let Some(connection) = tab.connection.take() {
            self.disconnect(connection);
        }
        drop(tab);
        if self.active == Some(id) {
            self.active = None;
            if let Some(tab) = self.tabs.get(index.min(self.tabs.len().saturating_sub(1))) {
                self.select_tab(tab.id, window, cx);
            } else {
                self.set_monitored_tab(None);
            }
        }
        cx.notify();
    }

    fn disconnect(&self, connection: Arc<Connection>) {
        self.runtime.spawn(async move {
            let _ = connection.disconnect().await;
        });
    }

    fn connect(
        &mut self,
        profile: ConnectionProfile,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self
            .tabs
            .iter()
            .filter(|tab| {
                tab.ssh_profile()
                    .is_some_and(|saved| saved.id == profile.id)
                    && matches!(tab.phase, Phase::Connected | Phase::Connecting)
            })
            .max_by_key(|tab| (Some(tab.id) == self.active, tab.phase == Phase::Connected))
        {
            self.select_tab(tab.id, window, cx);
            return;
        }
        self.connect_new(profile, password, window, cx);
    }

    fn connect_new(
        &mut self,
        mut profile: ConnectionProfile,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known_hosts = match config::known_hosts() {
            Ok(path) => path,
            Err(error) => {
                self.announce(format!("无法打开已知主机文件：{error:#}"), cx);
                return;
            }
        };
        self.settings.resolve_jump_references(&mut profile);
        let mut target = profile.clone();
        self.settings.apply_ssh_defaults(&mut target);
        let reuse_jumps = self.settings.reuse_jumps;
        self.start_tab(
            Session::Ssh(profile),
            password.is_some(),
            window,
            cx,
            move |id, initial, events| {
                run_ssh(
                    id,
                    target,
                    password,
                    known_hosts,
                    reuse_jumps,
                    initial,
                    events,
                )
            },
        );
    }

    fn request_reconnect(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id).filter(|tab| tab.can_reconnect()) else {
            return;
        };
        if tab.ssh_password_auth {
            if let Some(profile) = tab.ssh_profile().cloned() {
                self.reconnect_password_dialog(id, profile, window, cx);
            }
        } else {
            self.reconnect_ssh(id, None, window, cx);
        }
    }

    fn reconnect_ssh(
        &mut self,
        id: u64,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab(id).filter(|tab| tab.can_reconnect()) else {
            return;
        };
        let Some(mut profile) = tab.ssh_profile().cloned() else {
            return;
        };
        let known_hosts = match config::known_hosts() {
            Ok(path) => path,
            Err(error) => {
                self.announce(format!("无法打开已知主机文件：{error:#}"), cx);
                return;
            }
        };
        self.settings.resolve_jump_references(&mut profile);
        let mut target = profile.clone();
        self.settings.apply_ssh_defaults(&mut target);
        let reuse_jumps = self.settings.reuse_jumps;
        let session = Session::Ssh(profile);
        let initial = tab.link.size.get();
        let link = Rc::new(TerminalLink::default());
        link.size.set(initial);
        let (numbers, timestamps) = (tab.line_numbers, tab.timestamps);
        let (terminal, subscriptions) = self.build_terminal(id, &session, &link, window, cx);
        terminal.update(cx, |terminal, cx| {
            terminal.set_gutters(numbers, timestamps, cx);
            terminal.system_message("正在重新连接…", cx);
        });
        let operation_id = self.new_id();
        let tab = self.tab_mut(id).expect("retrying tab is still open");
        let previous = tab.operation.take();
        tab.operation_id = operation_id;
        tab.phase = Phase::Connecting;
        tab.session = session;
        tab.link = link;
        tab.terminal = terminal;
        tab.observed_terminal = TerminalObservation::default();
        tab.route.clear();
        tab._terminal_subscriptions = subscriptions;
        let events = self.events.clone();
        let operation = self.runtime.spawn(async move {
            // Finish closing the old transport and its forwarding listeners before rebinding.
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            run_ssh(
                operation_id,
                target,
                password,
                known_hosts,
                reuse_jumps,
                initial,
                events,
            )
            .await;
        });
        self.tab_mut(id)
            .expect("retrying tab is still open")
            .operation = Some(operation);
        self.notice.clear();
        self.select_tab(id, window, cx);
    }

    fn launch_local(&mut self, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        self.start_tab(
            Session::Local(shell),
            false,
            window,
            cx,
            move |id, initial, events| async move {
                let paths = if shell == LocalShell::Wsl {
                    local::wsl_paths()
                        .await
                        .map(Some)
                        .map_err(|error| format!("{error:#}"))
                } else {
                    Ok(None)
                };
                let wsl = paths.as_ref().ok().and_then(Option::as_ref);
                let (commands, output) =
                    match local::open(shell, initial.cols, initial.rows, wsl).await {
                        Ok(shell) => shell,
                        Err(error) => {
                            let _ = events
                                .send(Event::StartFailed(id, format!("{error:#}")))
                                .await;
                            return;
                        }
                    };
                if events
                    .send(Event::LocalStarted(id, commands, paths))
                    .await
                    .is_ok()
                {
                    relay_shell_output(id, output, &events).await;
                }
            },
        );
    }

    fn build_terminal(
        &self,
        id: u64,
        session: &Session,
        link: &Rc<TerminalLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TerminalView>, [Subscription; 2]) {
        let terminal = cx.new(|cx| {
            let link = link.clone();
            TerminalView::new(move |command| link.forward(command), cx)
        });
        terminal.update(cx, |terminal, cx| {
            terminal.set_light(self.settings.light, cx);
            terminal.set_compact(self.compact, cx);
            configure_terminal(
                terminal,
                &self.settings,
                &self.linux_highlights,
                session,
                cx,
            );
        });
        let terminal_subscription = cx.observe_in(&terminal, window, move |view, _, window, cx| {
            view.observe_terminal(id, window, cx);
        });
        let clipboard_subscription =
            cx.subscribe(&terminal, move |view, _, image: &ClipboardImage, cx| {
                view.upload_terminal_clipboard(id, image.bytes.clone(), image.extension, cx);
            });
        (terminal, [terminal_subscription, clipboard_subscription])
    }

    fn start_tab<F: Future<Output = ()> + Send + 'static>(
        &mut self,
        session: Session,
        ssh_password_auth: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        run: impl FnOnce(u64, TerminalSize, async_channel::Sender<Event>) -> F,
    ) {
        let id = self.new_id();
        let link = Rc::new(TerminalLink::default());
        let (terminal, terminal_subscriptions) =
            self.build_terminal(id, &session, &link, window, cx);
        let draft = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder(DRAFT_HINT)
        });
        let draft_subscription =
            cx.subscribe_in(&draft, window, move |view, _, event, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        shift: false,
                        secondary: false
                    }
                ) {
                    view.send_draft(id, window, cx);
                }
            });
        let initial = terminal.read(cx).size();
        let operation = self.runtime.spawn(run(id, initial, self.events.clone()));
        let (history_key, saved_history) = match &session {
            Session::Ssh(profile) => (
                format!("ssh:{}", profile.id),
                self.settings.sender_history.get(&profile.id),
            ),
            Session::Local(shell) => (format!("local:{}", shell.label()), None),
        };
        let command_history = self
            .sender_histories
            .entry(history_key)
            .or_insert_with(|| Rc::new(RefCell::new(saved_history.cloned().unwrap_or_default())))
            .clone();
        self.tabs.push(Tab {
            id,
            operation_id: id,
            session,
            ssh_password_auth,
            phase: Phase::Connecting,
            terminal,
            draft,
            link,
            line_numbers: self.settings.show_line_numbers,
            timestamps: self.settings.show_timestamps,
            show_sender: false,
            paste_mode: None,
            command_history,
            history_index: None,
            history_draft: String::new(),
            pending_notification: false,
            observed_terminal: TerminalObservation::default(),
            route: Vec::new(),
            connection: None,
            operation: Some(operation),
            monitor: None,
            monitor_state: monitor::MonitorState::default(),
            sample: None,
            files: FilePane::default(),
            _draft_subscription: draft_subscription,
            _terminal_subscriptions: terminal_subscriptions,
        });
        self.notice.clear();
        self.select_tab(id, window, cx);
    }

    fn test(
        &mut self,
        profile: ConnectionProfile,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.begin_test(profile, password, None, cx);
    }

    fn test_form(
        &mut self,
        profile: ConnectionProfile,
        form: Rc<dialogs::FormStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.begin_test(profile, None, Some(form.clone()), cx) {
            form.finish(self.notice.clone(), false, window);
        }
    }

    fn begin_test(
        &mut self,
        mut profile: ConnectionProfile,
        password: Option<String>,
        form: Option<Rc<dialogs::FormStatus>>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .tests
            .values()
            .any(|test| test.profile.id == profile.id)
        {
            self.announce("此会话正在测试连接", cx);
            return false;
        }
        let known_hosts = match config::known_hosts() {
            Ok(path) => path,
            Err(error) => {
                self.announce(format!("无法打开已知主机文件：{error:#}"), cx);
                return false;
            }
        };
        let id = self.new_id();
        self.settings.resolve_jump_references(&mut profile);
        let mut target = profile.clone();
        self.settings.apply_ssh_defaults(&mut target);
        target.ssh.port_forwards.clear();
        let reuse_jumps = self.settings.reuse_jumps;
        let events = self.events.clone();
        let task = self.runtime.spawn(async move {
            let result = establish(id, &target, password, known_hosts, reuse_jumps, &events).await;
            let result = match result {
                Ok(connection) => {
                    let _ = connection.disconnect().await;
                    Ok(())
                }
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = events.send(Event::TestFinished(id, result)).await;
        });
        self.announce(format!("正在测试 {}…", profile.name), cx);
        self.tests.insert(
            id,
            ConnectionTest {
                profile,
                started: Instant::now(),
                hops: Vec::new(),
                form,
                _task: task.abort_handle(),
            },
        );
        true
    }

    fn test_progress(&self, profile: &str, hop: usize) -> Option<&HopProgress> {
        let active = self.tests.values().find(|test| test.profile.id == profile);
        let route = if let Some(test) = active {
            &test.hops
        } else {
            &self.recent_tests.iter().find(|(id, _)| id == profile)?.1
        };
        route.iter().find(|progress| progress.index == hop)
    }

    fn send_draft(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else { return };
        if tab.phase != Phase::Connected || (tab.terminal.read(cx).raw() && !tab.show_sender) {
            return;
        }
        let command = tab.draft.read(cx).value();
        if command.trim().is_empty() {
            return;
        }
        let execute = !tab
            .paste_mode
            .unwrap_or_else(|| tab.terminal.read(cx).agent_name().is_some());
        let receivers = self.draft_receivers(id);
        if receivers.is_empty() {
            self.announce("请选择至少一个已连接的发送目标", cx);
            return;
        }
        if !execute
            && command.chars().any(char::is_control)
            && let Some(receiver) = receivers
                .iter()
                .filter_map(|id| self.tab(*id))
                .find(|tab| !tab.terminal.read(cx).bracketed_paste_enabled())
        {
            self.announce(
                format!(
                    "{} 尚未启用括号粘贴，无法安全发送含换行或控制字符的内容",
                    receiver.name()
                ),
                cx,
            );
            return;
        }
        for receiver in receivers {
            if let Some(tab) = self.tab(receiver) {
                tab.terminal.update(cx, |terminal, cx| {
                    terminal.send_draft(&command, execute, cx)
                });
            }
        }
        let history_update = {
            let tab = self.tab_mut(id).expect("sender tab is still open");
            tab.history_index = None;
            tab.history_draft.clear();
            tab.draft.update(cx, |draft, cx| draft.clean(window, cx));
            let mut history = tab.command_history.borrow_mut();
            if command.len() <= SENDER_HISTORY_ENTRY_BYTES
                && history.front().is_none_or(|last| last != command.as_ref())
            {
                history.push_front(command.to_string());
                history.truncate(SENDER_HISTORY_LIMIT);
                tab.ssh_profile()
                    .map(|profile| (profile.id.clone(), history.clone()))
            } else {
                None
            }
        };
        if let Some((id, history)) = history_update
            && self
                .settings
                .profiles
                .iter()
                .any(|profile| profile.id == id)
        {
            let mut settings = self.settings.clone();
            settings.sender_history.insert(id, history);
            self.persist(settings, cx);
        }
    }

    fn apply_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Trust(id, request) => {
                if self.tab_for_operation(id).is_some() || self.tests.contains_key(&id) {
                    match request {
                        ConnectionPrompt::HostKey(request) => {
                            self.prompt_trust(request, window, cx)
                        }
                        ConnectionPrompt::ProxyCommand(request) => {
                            self.prompt_proxy_command(request, window, cx)
                        }
                    }
                }
            }
            Event::Connected(operation_id, connection, commands) => {
                let Some(tab) = self.tab_mut_for_operation(operation_id) else {
                    self.disconnect(connection);
                    return;
                };
                let id = tab.id;
                tab.connection = Some(connection.clone());
                tab.bind_shell(ShellInput::Ssh(commands));
                for rule in connection.forwardings() {
                    tab.terminal.update(cx, |terminal, cx| {
                        terminal.system_message(format!("已启用端口转发 · {rule}"), cx);
                    });
                }
                let monitor = self.runtime.spawn(monitor::sample_host(
                    operation_id,
                    connection,
                    self.monitored_operation.subscribe(),
                    self.monitor_interval.clone(),
                    self.events.clone(),
                ));
                if let Some(tab) = self.tab_mut(id) {
                    tab.monitor = Some(monitor.abort_handle());
                }
                self.list(id, ".".into(), cx);
                cx.notify();
            }
            Event::LocalStarted(id, commands, paths) => {
                if let Some(tab) = self.tab_mut(id) {
                    tab.bind_shell(ShellInput::Local(commands));
                    self.start_local_files(id, paths, cx);
                    cx.notify();
                }
            }
            Event::StartFailed(operation_id, error) => {
                self.finish_tab(operation_id, Phase::Failed, &error, cx);
            }
            Event::TestFinished(id, result) => {
                if let Some(test) = self.tests.remove(&id) {
                    let ok = result.is_ok();
                    let name = &test.profile.name;
                    let times = test
                        .hops
                        .iter()
                        .map(|hop| hop.rtt_ms)
                        .collect::<Option<Vec<_>>>();
                    let notice = match result {
                        Ok(())
                            if times.as_ref().is_some_and(|times| {
                                times.len() == test.profile.jumps.len() + 1
                            }) =>
                        {
                            let rtt: u64 = times.into_iter().flatten().sum();
                            format!("连接测试通过 · {name} · 全链路往返 {rtt} ms")
                        }
                        Ok(()) => format!(
                            "连接测试通过 · {name} · 建立用时 {} ms",
                            test.started.elapsed().as_millis()
                        ),
                        Err(error) => format!("连接测试失败 · {name} · {error}"),
                    };
                    self.recent_tests.retain(|(id, _)| id != &test.profile.id);
                    self.recent_tests.push_front((test.profile.id, test.hops));
                    self.recent_tests.truncate(64);
                    if let Some(form) = test.form {
                        form.finish(notice.clone(), ok, window);
                    }
                    self.announce(notice, cx);
                }
            }
            Event::Route(operation_id, progress) => {
                if let Some(test) = self.tests.get_mut(&operation_id) {
                    record_hop(&mut test.hops, progress);
                    if test.form.is_some() {
                        window.refresh();
                    }
                    return;
                }
                if let Some(tab) = self.tab_mut_for_operation(operation_id) {
                    let node = tab
                        .ssh_profile()
                        .map(|profile| {
                            profile
                                .jumps
                                .get(progress.index)
                                .map(|hop| hop.host.as_str())
                                .unwrap_or(&profile.host)
                        })
                        .unwrap_or("SSH");
                    let step = match progress.stage {
                        HopStage::Connecting => "正在建立连接",
                        HopStage::ProxyApproval => "等待确认代理命令",
                        HopStage::Trust => "等待确认主机密钥",
                        HopStage::Authenticating => "正在认证",
                        HopStage::Connected => "连接已建立",
                        HopStage::Failed => "连接失败",
                    };
                    let mut message = format!("第 {} 跳 · {node} · {step}", progress.index + 1);
                    if let Some(rtt) = progress.rtt_ms {
                        message.push_str(&format!(" · {rtt} ms"));
                    }
                    if let Some(error) = progress.error.as_deref() {
                        message.push_str(&format!(" · {error}"));
                    }
                    tab.terminal
                        .update(cx, |terminal, cx| terminal.system_message(message, cx));
                    record_hop(&mut tab.route, progress);
                    cx.notify();
                }
            }
            Event::Shell(operation_id, ShellEvent::Output(bytes)) => {
                if let Some(tab) = self.tab_for_operation(operation_id) {
                    tab.terminal
                        .update(cx, |terminal, cx| terminal.feed(&bytes, cx));
                }
            }
            Event::Shell(operation_id, ShellEvent::Exited(code)) => {
                let Some(tab) = self.tab_for_operation(operation_id) else {
                    return;
                };
                let reason = match (tab.is_local(), code) {
                    (true, Some(code)) => format!("本地 Shell 已退出 · 退出码 {code}"),
                    (true, None) => "本地 Shell 已关闭".into(),
                    (false, Some(code)) => format!("SSH 会话已退出 · 退出码 {code}"),
                    (false, None) => "SSH 通道已关闭".into(),
                };
                self.finish_tab(operation_id, Phase::Disconnected, &reason, cx);
            }
            Event::Shell(operation_id, ShellEvent::Error(error)) => {
                self.finish_tab(operation_id, Phase::Failed, &error, cx);
            }
            Event::Sample(operation_id, sample) => {
                if let Some(tab) = self
                    .tab_mut_for_operation(operation_id)
                    .filter(|tab| tab.phase == Phase::Connected)
                {
                    let id = tab.id;
                    let sample = Arc::new(sample);
                    tab.monitor_state.push(sample.clone());
                    tab.sample = Some(sample);
                    if self.active == Some(id) {
                        cx.notify();
                    }
                }
            }
            Event::File(operation_id, event) => {
                self.apply_file_event(operation_id, event, window, cx)
            }
            Event::DragFinished(notice) => self.announce(notice, cx),
        }
    }

    fn finish_tab(
        &mut self,
        operation_id: u64,
        phase: Phase,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .tab_for_operation(operation_id)
            .filter(|tab| matches!(tab.phase, Phase::Connecting | Phase::Connected))
        else {
            return;
        };
        let id = tab.id;
        self.cancel_file_transfers(id, cx);
        let Some(tab) = self.tab_mut(id) else { return };
        tab.phase = phase;
        tab.link.commands.borrow_mut().take();
        if let Some(monitor) = tab.monitor.take() {
            monitor.abort();
        }
        tab.sample = None;
        tab.monitor_state.clear();
        tab.files = FilePane::default();
        tab.connection = None;
        let reason = reason.replace(['\r', '\n', '\x1b'], " ");
        tab.terminal.update(cx, |terminal, cx| {
            terminal.end_session(cx);
            terminal.system_message(reason.clone(), cx);
        });
        let notice = format!("{} · {reason}", tab.name());
        self.announce(notice, cx);
    }
}

fn record_hop(route: &mut Vec<HopProgress>, progress: HopProgress) {
    if let Some(existing) = route.iter_mut().find(|hop| hop.index == progress.index) {
        *existing = progress;
    } else {
        route.push(progress);
    }
}

fn configure_terminal(
    terminal: &mut TerminalView,
    settings: &Settings,
    linux_highlights: &Rc<LinuxHighlights>,
    session: &Session,
    cx: &mut Context<TerminalView>,
) {
    let family = Theme::global(cx).mono_font_family.clone();
    terminal.set_font_family(Some(family.as_str()), cx);
    terminal.set_gutters(settings.show_line_numbers, settings.show_timestamps, cx);
    terminal.set_font_metrics(
        settings.terminal_font_size,
        settings.terminal_line_height,
        cx,
    );
    terminal.set_timeline_options(
        settings.timestamp_format.into(),
        Duration::from_secs(settings.idle_threshold.max(1)),
        settings.dim_repeated_timestamps,
        cx,
    );
    let highlights = (settings.linux_highlighting
        && matches!(session, Session::Ssh(_) | Session::Local(LocalShell::Wsl)))
    .then(|| linux_highlights.clone());
    terminal.set_linux_highlights(highlights, cx);
}

async fn run_ssh(
    id: u64,
    target: ConnectionProfile,
    password: Option<String>,
    known_hosts: std::path::PathBuf,
    reuse_jumps: bool,
    initial: TerminalSize,
    events: async_channel::Sender<Event>,
) {
    match establish(id, &target, password, known_hosts, reuse_jumps, &events).await {
        Ok(connection) => run_shell(id, Arc::new(connection), initial, &events).await,
        Err(error) => {
            let _ = events
                .send(Event::StartFailed(id, format!("{error:#}")))
                .await;
        }
    }
}

async fn establish(
    id: u64,
    profile: &ConnectionProfile,
    password: Option<String>,
    known_hosts: std::path::PathBuf,
    reuse_jumps: bool,
    events: &async_channel::Sender<Event>,
) -> anyhow::Result<Connection> {
    let (sender, mut requests) = mpsc::unbounded_channel();
    let (route_tx, mut route_rx) = mpsc::unbounded_channel();
    let connect = Connection::connect_with_progress(
        profile,
        password,
        known_hosts,
        sender,
        reuse_jumps,
        move |progress| {
            let _ = route_tx.send(progress);
        },
    );
    tokio::pin!(connect);
    loop {
        tokio::select! {
            result = &mut connect => {
                while let Ok(progress) = route_rx.try_recv() {
                    events.send(Event::Route(id, progress)).await?;
                }
                return result;
            },
            Some(request) = requests.recv() => {
                while let Ok(progress) = route_rx.try_recv() {
                    events.send(Event::Route(id, progress)).await?;
                }
                events.send(Event::Trust(id, request)).await?;
            },
            Some(progress) = route_rx.recv() => events.send(Event::Route(id, progress)).await?,
        }
    }
}

async fn run_shell(
    id: u64,
    connection: Arc<Connection>,
    initial: TerminalSize,
    events: &async_channel::Sender<Event>,
) {
    let (command_sender, output) = match connection.shell(initial.cols, initial.rows).await {
        Ok(shell) => shell,
        Err(error) => {
            let _ = connection.disconnect().await;
            let _ = events
                .send(Event::StartFailed(id, format!("{error:#}")))
                .await;
            return;
        }
    };
    let (sender, mut input) = mpsc::unbounded_channel();
    if events
        .send(Event::Connected(id, connection.clone(), sender))
        .await
        .is_err()
    {
        let _ = connection.disconnect().await;
        return;
    }
    // The bounded backend channel may fill while a large paste is written. Keep
    // receiving output while waiting for capacity so neither direction stalls.
    let commands = async {
        while let Some(command) = input.recv().await {
            if command_sender.send(command).await.is_err() {
                break;
            }
        }
    };
    let shell_output = relay_shell_output(id, output, events);
    tokio::pin!(shell_output);
    tokio::select! {
        _ = commands => {
            let _ = command_sender.try_send(ShellCommand::Close);
            if tokio::time::timeout(Duration::from_millis(250), &mut shell_output).await.is_err() {
                let _ = events.send(Event::Shell(id, ShellEvent::Exited(None))).await;
            }
        }
        _ = &mut shell_output => {}
    }
    let _ = connection.disconnect().await;
}

async fn relay_shell_output(
    id: u64,
    mut output: mpsc::Receiver<ShellEvent>,
    events: &async_channel::Sender<Event>,
) {
    let mut pending = None;
    loop {
        let event = match pending.take() {
            Some(event) => event,
            None => match output.recv().await {
                Some(event) => event,
                None => break,
            },
        };
        let event = match event {
            ShellEvent::Output(mut bytes) => {
                while bytes.len() < 256 * 1024 {
                    match output.try_recv() {
                        Ok(ShellEvent::Output(more)) => bytes.extend_from_slice(&more),
                        Ok(next) => {
                            pending = Some(next);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                ShellEvent::Output(bytes)
            }
            other => other,
        };
        let finished = matches!(event, ShellEvent::Exited(_) | ShellEvent::Error(_));
        if events.send(Event::Shell(id, event)).await.is_err() || finished {
            return;
        }
    }
    let _ = events
        .send(Event::Shell(id, ShellEvent::Exited(None)))
        .await;
}
