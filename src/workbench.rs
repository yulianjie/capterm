mod dialogs;
mod files;
mod ui;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::TitleBar;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::config::{self, ConnectionProfile, Settings};
use crate::local::{self, LocalShell};
use crate::ssh::{Connection, Metrics, ShellCommand, ShellEvent, TrustRequest};
use crate::terminal::{TerminalCommand, TerminalSize, TerminalView};
use files::{FileDrag, FileEvent, FilePane};

const DRAFT_HINT: &str = "Send a command to this session…";

pub fn open(cx: &mut App) {
    let (settings, load_error) = match Settings::load() {
        Ok(settings) => (settings, None),
        Err(error) => (
            Settings::default(),
            Some(format!("Cannot load settings: {error:#}")),
        ),
    };
    ui::apply_theme(settings.light, cx);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("capterm-session")
        .enable_all()
        .build()
        .expect("start session runtime");
    let window_bounds = Bounds::centered(None, size(px(1360.), px(840.)), cx);
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
            (Self::Connecting, true) => "Starting",
            (Self::Connected, true) => "Running",
            (Self::Disconnected, true) => "Exited",
            (Self::Failed, true) => "Shell failed",
            (Self::Connecting, false) => "Connecting",
            (Self::Connected, false) => "Connected",
            (Self::Disconnected, false) => "Disconnected",
            (Self::Failed, false) => "Connection failed",
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Drawer {
    Files,
    Sessions,
}

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
    session: Session,
    phase: Phase,
    terminal: Entity<TerminalView>,
    draft: Entity<InputState>,
    link: Rc<TerminalLink>,
    gutter: bool,
    manual_raw: bool,
    connection: Option<Arc<Connection>>,
    operation: AbortHandle,
    monitor: Option<AbortHandle>,
    sample: Option<(Metrics, Instant)>,
    files: FilePane,
    _subscriptions: Vec<Subscription>,
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

    fn is_local(&self) -> bool {
        matches!(self.session, Session::Local(_))
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
        self.operation.abort();
        if let Some(monitor) = self.monitor.take() {
            monitor.abort();
        }
    }
}

enum Event {
    Trust(u64, TrustRequest),
    Connected(u64, Arc<Connection>, mpsc::UnboundedSender<ShellCommand>),
    LocalStarted(u64, std::sync::mpsc::Sender<ShellCommand>),
    StartFailed(u64, String),
    TestFinished(u64, Result<(), String>),
    Shell(u64, ShellEvent),
    Sample(u64, Option<Metrics>),
    File(u64, FileEvent),
    DragFinished(String),
}

struct Workbench {
    settings: Settings,
    load_error: Option<String>,
    events: async_channel::Sender<Event>,
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
    filter: Entity<InputState>,
    empty_draft: Entity<InputState>,
    raw_draft: Entity<InputState>,
    drawer: Option<Drawer>,
    compact: bool,
    notice: String,
    tests: HashMap<u64, (String, AbortHandle)>,
    file_drag: Option<FileDrag>,
    _pump: Task<()>,
    _filter_subscription: Subscription,
    runtime: tokio::runtime::Runtime,
}

impl Workbench {
    fn new(
        settings: Settings,
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
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter sessions…"));
        let filter_subscription = cx.subscribe(&filter, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            notice: load_error.clone().unwrap_or_default(),
            settings,
            load_error,
            events,
            tabs: Vec::new(),
            active: None,
            next_id: 0,
            filter,
            empty_draft: cx.new(|cx| InputState::new(window, cx).placeholder(DRAFT_HINT)),
            raw_draft: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Keyboard goes directly to the terminal…")
            }),
            drawer: None,
            compact: window.viewport_size().width < px(760.),
            tests: HashMap::new(),
            file_drag: None,
            _pump: pump,
            _filter_subscription: filter_subscription,
            runtime,
        }
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

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn announce(&mut self, notice: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = notice.into();
        cx.notify();
    }

    fn persist(&mut self, settings: Settings, cx: &mut Context<Self>) -> bool {
        let result = match &self.load_error {
            Some(error) => Err(anyhow::anyhow!("{error}")),
            None => settings.save(),
        };
        match result {
            Ok(()) => {
                self.settings = settings;
                cx.notify();
                true
            }
            Err(error) => {
                self.announce(format!("Cannot save settings: {error:#}"), cx);
                false
            }
        }
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.settings.clone();
        settings.light = !settings.light;
        let light = settings.light;
        if self.persist(settings, cx) {
            ui::apply_theme(light, cx);
            for tab in &self.tabs {
                tab.terminal
                    .update(cx, |terminal, cx| terminal.set_light(light, cx));
            }
        }
    }

    fn select_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab(id) {
            tab.terminal
                .update(cx, |terminal, cx| terminal.focus(window, cx));
            self.active = Some(id);
            self.drawer = None;
            cx.notify();
        }
    }

    fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let mut tab = self.tabs.remove(index);
        if let Some(connection) = tab.connection.take() {
            self.disconnect(connection);
        }
        drop(tab);
        if self.active == Some(id) {
            self.active = None;
            if let Some(tab) = self.tabs.get(index.min(self.tabs.len().saturating_sub(1))) {
                self.select_tab(tab.id, window, cx);
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
        if let Some(tab) = self.tabs.iter().find(|tab| {
            tab.ssh_profile()
                .is_some_and(|connected| connected.id == profile.id)
        }) {
            if matches!(tab.phase, Phase::Connected | Phase::Connecting) {
                self.select_tab(tab.id, window, cx);
                return;
            }
            self.close_tab(tab.id, window, cx);
        }
        let known_hosts = match config::known_hosts() {
            Ok(path) => path,
            Err(error) => {
                self.announce(format!("Cannot open known hosts: {error:#}"), cx);
                return;
            }
        };
        let target = profile.clone();
        self.start_tab(
            Session::Ssh(profile),
            window,
            cx,
            |id, initial, events| async move {
                let result = establish(id, &target, password, known_hosts, &events).await;
                match result {
                    Ok(connection) => run_shell(id, Arc::new(connection), initial, &events).await,
                    Err(error) => {
                        let _ = events
                            .send(Event::StartFailed(id, format!("{error:#}")))
                            .await;
                    }
                }
            },
        );
    }

    fn launch_local(&mut self, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        self.start_tab(
            Session::Local(shell),
            window,
            cx,
            move |id, initial, events| async move {
                let (commands, output) = match local::open(shell, initial.cols, initial.rows).await
                {
                    Ok(shell) => shell,
                    Err(error) => {
                        let _ = events
                            .send(Event::StartFailed(id, format!("{error:#}")))
                            .await;
                        return;
                    }
                };
                if events.send(Event::LocalStarted(id, commands)).await.is_ok() {
                    relay_shell_output(id, output, &events).await;
                }
            },
        );
    }

    fn start_tab<F: Future<Output = ()> + Send + 'static>(
        &mut self,
        session: Session,
        window: &mut Window,
        cx: &mut Context<Self>,
        run: impl FnOnce(u64, TerminalSize, async_channel::Sender<Event>) -> F,
    ) {
        let id = self.new_id();
        let link = Rc::new(TerminalLink {
            size: Cell::new(TerminalSize::default()),
            commands: RefCell::new(None),
        });
        let terminal = cx.new(|cx| {
            let link = link.clone();
            TerminalView::new(move |command| link.forward(command), cx)
        });
        terminal.update(cx, |terminal, cx| {
            terminal.set_light(self.settings.light, cx);
            terminal.set_compact(self.compact, cx);
            terminal.set_gutter(true, cx);
        });
        let draft = cx.new(|cx| InputState::new(window, cx).placeholder(DRAFT_HINT));
        let draft_subscription =
            cx.subscribe_in(&draft, window, move |view, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    view.send_draft(id, window, cx);
                }
            });
        let terminal_subscription = cx.observe(&terminal, |_, _, cx| cx.notify());
        let initial = terminal.read(cx).size();
        let operation = self.runtime.spawn(run(id, initial, self.events.clone()));
        self.tabs.push(Tab {
            id,
            session,
            phase: Phase::Connecting,
            terminal,
            draft,
            link,
            gutter: true,
            manual_raw: false,
            connection: None,
            operation: operation.abort_handle(),
            monitor: None,
            sample: None,
            files: FilePane::default(),
            _subscriptions: vec![draft_subscription, terminal_subscription],
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
        if self.tests.values().any(|(id, _)| *id == profile.id) {
            return;
        }
        let known_hosts = match config::known_hosts() {
            Ok(path) => path,
            Err(error) => {
                self.announce(format!("Cannot open known hosts: {error:#}"), cx);
                return;
            }
        };
        let id = self.new_id();
        let target = profile.clone();
        let events = self.events.clone();
        let task = self.runtime.spawn(async move {
            let result = establish(id, &target, password, known_hosts, &events).await;
            let result = match result {
                Ok(connection) => {
                    let _ = connection.disconnect().await;
                    Ok(())
                }
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = events.send(Event::TestFinished(id, result)).await;
        });
        self.tests.insert(id, (profile.id, task.abort_handle()));
        self.announce(format!("Testing {}…", profile.name), cx);
    }

    fn send_draft(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else { return };
        if tab.phase != Phase::Connected || tab.terminal.read(cx).raw() {
            return;
        }
        let command = tab.draft.read(cx).value();
        if command.trim().is_empty() {
            return;
        }
        tab.terminal
            .update(cx, |terminal, cx| terminal.send_draft(&command, true, cx));
        tab.draft.update(cx, |draft, cx| draft.clean(window, cx));
    }

    fn apply_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Trust(id, request) => {
                if self.tab(id).is_some() || self.tests.contains_key(&id) {
                    self.prompt_trust(request, window, cx);
                }
            }
            Event::Connected(id, connection, commands) => {
                let Some(tab) = self.tab_mut(id) else {
                    self.disconnect(connection);
                    return;
                };
                tab.connection = Some(connection.clone());
                tab.bind_shell(ShellInput::Ssh(commands));
                let events = self.events.clone();
                let monitor = self.runtime.spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(5));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        interval.tick().await;
                        let result = connection.metrics().await.ok();
                        if events.send(Event::Sample(id, result)).await.is_err() {
                            break;
                        }
                    }
                });
                if let Some(tab) = self.tab_mut(id) {
                    tab.monitor = Some(monitor.abort_handle());
                }
                self.list(id, ".".into(), cx);
            }
            Event::LocalStarted(id, commands) => {
                if let Some(tab) = self.tab_mut(id) {
                    tab.bind_shell(ShellInput::Local(commands));
                    cx.notify();
                }
            }
            Event::StartFailed(id, error) => self.finish_tab(id, Phase::Failed, &error, cx),
            Event::TestFinished(id, result) => {
                if let Some((profile, _)) = self.tests.remove(&id) {
                    let name = self
                        .settings
                        .profiles
                        .iter()
                        .find(|saved| saved.id == profile)
                        .map(|saved| saved.name.as_str())
                        .unwrap_or(&profile);
                    let notice = match result {
                        Ok(()) => format!("Connection test passed · {name}"),
                        Err(error) => format!("Connection test failed · {name} · {error}"),
                    };
                    self.announce(notice, cx);
                }
            }
            Event::Shell(id, ShellEvent::Output(bytes)) => {
                if let Some(tab) = self.tab(id) {
                    tab.terminal
                        .update(cx, |terminal, cx| terminal.feed(&bytes, cx));
                }
            }
            Event::Shell(id, ShellEvent::Exited(code)) => {
                let reason = match (self.tab(id).is_some_and(Tab::is_local), code) {
                    (true, Some(code)) => format!("Shell exited · {code}"),
                    (true, None) => "Shell closed".into(),
                    (false, Some(code)) => format!("SSH exited · {code}"),
                    (false, None) => "SSH channel closed".into(),
                };
                self.finish_tab(id, Phase::Disconnected, &reason, cx);
            }
            Event::Shell(id, ShellEvent::Error(error)) => {
                self.finish_tab(id, Phase::Failed, &error, cx)
            }
            Event::Sample(id, sample) => {
                if let Some(tab) = self.tab_mut(id).filter(|tab| tab.phase == Phase::Connected) {
                    tab.sample = sample.map(|sample| (sample, Instant::now()));
                    cx.notify();
                }
            }
            Event::File(id, event) => self.apply_file_event(id, event, window, cx),
            Event::DragFinished(notice) => self.announce(notice, cx),
        }
    }

    fn finish_tab(&mut self, id: u64, phase: Phase, reason: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        if !matches!(tab.phase, Phase::Connecting | Phase::Connected) {
            return;
        }
        tab.phase = phase;
        tab.link.commands.borrow_mut().take();
        if let Some(monitor) = tab.monitor.take() {
            monitor.abort();
        }
        tab.sample = None;
        tab.files = FilePane::default();
        let connection = tab.connection.take();
        let reason = reason.replace(['\r', '\n', '\x1b'], " ");
        let notice = format!("{} · {reason}", tab.name());
        if let Some(connection) = connection {
            self.disconnect(connection);
        }
        self.announce(notice, cx);
    }
}

async fn establish(
    id: u64,
    profile: &ConnectionProfile,
    password: Option<String>,
    known_hosts: std::path::PathBuf,
    events: &async_channel::Sender<Event>,
) -> anyhow::Result<Connection> {
    let (sender, mut requests) = mpsc::unbounded_channel();
    let connect = Connection::connect(profile, password, known_hosts, sender);
    tokio::pin!(connect);
    loop {
        tokio::select! {
            result = &mut connect => return result,
            Some(request) = requests.recv() => events.send(Event::Trust(id, request)).await?,
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
    while let Some(event) = output.recv().await {
        let finished = matches!(event, ShellEvent::Exited(_) | ShellEvent::Error(_));
        if events.send(Event::Shell(id, event)).await.is_err() || finished {
            return;
        }
    }
    let _ = events
        .send(Event::Shell(id, ShellEvent::Exited(None)))
        .await;
}
