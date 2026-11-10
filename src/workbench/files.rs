mod browser;
mod editor;
mod local;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputState, Rope};
use gpui_kit::*;
use tokio::sync::watch;
use tokio::task::AbortHandle;

pub(super) use editor::FileEditor;

use super::{Event, Phase, Session, Tab, Workbench};
use crate::config::TransferConflictPreference;
use crate::file_text::{FileText, SaveTextResult, TextEncoding};
use crate::local::{LocalShell, WslPaths};
use crate::ssh::{
    Connection, RemoteEntry, RemoteMutation, TransferChoice, TransferConflict, TransferOptions,
    TransferOutcome, TransferSignal,
};

const CLIPBOARD_DIRECTORY: &str = "/tmp/capterm";

enum FileSource {
    Ssh(Arc<Connection>),
    Local,
}

impl FileSource {
    fn for_tab(tab: &Tab) -> Option<Self> {
        if !tab.has_file_browser() {
            return None;
        }
        if tab.is_local() {
            Some(Self::Local)
        } else {
            tab.connection.clone().map(Self::Ssh)
        }
    }

    async fn list(&self, directory: &str) -> anyhow::Result<(String, Vec<RemoteEntry>)> {
        match self {
            Self::Ssh(connection) => connection.list(directory).await,
            Self::Local => local::list(directory).await,
        }
    }

    async fn open_text(
        &self,
        path: &str,
        encoding: TextEncoding,
        edit_limit_mb: u64,
    ) -> anyhow::Result<Option<FileText>> {
        match self {
            Self::Ssh(connection) => {
                connection
                    .open_remote_text(path, encoding, edit_limit_mb)
                    .await
            }
            Self::Local => local::open_text(path, encoding, edit_limit_mb).await,
        }
    }

    async fn save_text(
        &self,
        path: &str,
        original: &FileText,
        edited: &str,
        force: bool,
    ) -> anyhow::Result<SaveTextResult> {
        match self {
            Self::Ssh(connection) => {
                connection
                    .save_remote_text(path, original, edited, force)
                    .await
            }
            Self::Local => local::save_text(path, original, edited, force).await,
        }
    }
}

#[derive(Default)]
pub(super) struct FilePane {
    directory: String,
    home: String,
    wsl: Option<WslPaths>,
    terminal_directory: Option<String>,
    osc_directory: bool,
    directory_probe_active: bool,
    directory_probe_pending: bool,
    entries: Vec<RemoteEntry>,
    branches: HashMap<String, Option<Vec<RemoteEntry>>>,
    visible_rows: std::cell::RefCell<Option<browser::VisibleFileRows>>,
    selected: Option<RemoteEntry>,
    loading: bool,
    error: Option<String>,
    listing: u64,
    navigation: Option<String>,
    pending_editors: HashMap<String, PendingEditor>,
    follow_disabled: bool,
    hide_hidden: bool,
    filter_open: bool,
    filter: Option<(Entity<InputState>, Subscription)>,
    drop_hover: Option<(Point<Pixels>, String, usize)>,
    native_drag: Option<RemoteEntry>,
    transfers_open: bool,
    transfers: Vec<Transfer>,
    conflict_all: bool,
    conflict_choices: HashMap<u64, TransferChoice>,
    local_previews: Vec<tempfile::TempDir>,
}

enum PathTarget {
    File,
    Folder,
}

impl FilePane {
    fn insertion_path(&self, target: PathTarget) -> Option<&str> {
        if self.directory.is_empty() {
            return None;
        }
        let selected = self.selected.as_ref();
        match target {
            PathTarget::File => selected
                .filter(|entry| !entry.is_dir)
                .map(|entry| entry.path.as_str()),
            PathTarget::Folder => Some(
                selected
                    .filter(|entry| entry.is_dir)
                    .map_or(self.directory.as_str(), |entry| entry.path.as_str()),
            ),
        }
    }
}

struct PendingEditor {
    id: u64,
    encoding: TextEncoding,
    open_external: bool,
    task: AbortHandle,
}

impl Drop for PendingEditor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
pub(super) struct FileDrag {
    tab: u64,
    entry: RemoteEntry,
}

#[derive(Clone)]
enum TransferSource {
    Upload {
        local: PathBuf,
        directory: String,
        insert: bool,
    },
    Clipboard(PathBuf),
    Download {
        remote: String,
        local: PathBuf,
        open: bool,
    },
}

impl TransferSource {
    fn destination(&self) -> String {
        match self {
            Self::Upload { directory, .. } => directory.clone(),
            Self::Clipboard(_) => CLIPBOARD_DIRECTORY.to_owned(),
            Self::Download { local, .. } => local.to_string_lossy().into_owned(),
        }
    }

    fn is_upload(&self) -> bool {
        matches!(self, Self::Upload { .. } | Self::Clipboard(_))
    }
}

enum TransferState {
    Waiting,
    Running,
    Paused,
    Cancelling,
    Conflict(TransferConflict),
    Complete(String),
    Failed(String),
    Cancelled,
    Skipped,
}

impl TransferState {
    fn pending(&self) -> bool {
        matches!(
            self,
            Self::Waiting | Self::Running | Self::Paused | Self::Cancelling | Self::Conflict(_)
        )
    }
}

struct TransferHandle {
    abort: AbortHandle,
    signal: watch::Sender<TransferSignal>,
}

impl Drop for TransferHandle {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

struct Transfer {
    id: u64,
    batch: u64,
    name: String,
    source: TransferSource,
    state: TransferState,
    options: TransferOptions,
    handle: Option<TransferHandle>,
    copied: u64,
    total: u64,
    speed: f64,
    measured: Instant,
}

pub(super) enum FileEvent {
    List(u64, Result<(String, Vec<RemoteEntry>), String>),
    Branch(u64, String, Result<Vec<RemoteEntry>, String>),
    Directory {
        prompt: u64,
        osc: u64,
        path: Option<String>,
    },
    Text(u64, RemoteEntry, Result<Option<FileText>, String>),
    ClipboardReady(Result<(tempfile::TempDir, PathBuf), String>),
    Saved(u64, Rope, Result<SaveTextResult, String>),
    Mutation(Result<(), String>),
    Progress(u64, u64, u64),
    Transfer(u64, u64, Result<TransferOutcome, String>),
}

fn report_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

impl Workbench {
    pub(super) fn list(&mut self, id: u64, mut directory: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(source) = FileSource::for_tab(tab) else {
            return;
        };
        if directory == "." && matches!(tab.session, Session::Local(LocalShell::Wsl)) {
            let Some(home) = tab
                .files
                .wsl
                .as_ref()
                .and_then(|wsl| wsl.windows_directory(wsl.home()))
            else {
                return;
            };
            directory = home;
        }
        tab.files.listing += 1;
        tab.files.loading = true;
        tab.files.error = None;
        tab.files.navigation = Some(directory.clone());
        let listing = tab.files.listing;
        let operation_id = tab.operation_id;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = source.list(&directory).await.map_err(report_error);
            let _ = events
                .send(Event::File(operation_id, FileEvent::List(listing, result)))
                .await;
        });
        cx.notify();
    }

    fn toggle_directory(&mut self, id: u64, entry: RemoteEntry, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(source) = FileSource::for_tab(tab) else {
            return;
        };
        tab.files.selected = Some(entry.clone());
        *tab.files.visible_rows.get_mut() = None;
        if tab.files.branches.remove(&entry.path).is_some() {
            let local = tab.is_local();
            let remote_prefix = format!("{}/", entry.path);
            tab.files.branches.retain(|path, _| {
                if local {
                    !Path::new(path).starts_with(&entry.path)
                } else {
                    !path.starts_with(&remote_prefix)
                }
            });
        } else {
            tab.files.branches.insert(entry.path.clone(), None);
            let listing = tab.files.listing;
            let operation_id = tab.operation_id;
            let events = self.events.clone();
            self.runtime.spawn(async move {
                let result = source
                    .list(&entry.path)
                    .await
                    .map(|(_, entries)| entries)
                    .map_err(report_error);
                let _ = events
                    .send(Event::File(
                        operation_id,
                        FileEvent::Branch(listing, entry.path, result),
                    ))
                    .await;
            });
        }
        cx.notify();
    }

    pub(super) fn start_local_files(
        &mut self,
        id: u64,
        paths: Result<Option<WslPaths>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id) else { return };
        match paths {
            Ok(Some(wsl)) => {
                tab.files.terminal_directory = wsl.windows_directory(wsl.home());
                tab.files.wsl = Some(wsl);
            }
            Ok(None) => {}
            Err(error) => {
                tab.files.error = Some(error);
                return;
            }
        }
        self.list(id, ".".into(), cx);
    }

    pub(super) fn file_terminal_directory(
        &mut self,
        id: u64,
        directory: String,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id).filter(|tab| tab.has_file_browser()) else {
            return;
        };
        let path = match tab.session {
            Session::Local(LocalShell::Wsl) => tab
                .files
                .wsl
                .as_ref()
                .and_then(|wsl| wsl.windows_directory(&directory)),
            Session::Local(_) => local::osc_directory(&directory),
            Session::Ssh(_) => {
                (directory.starts_with('/') && !directory.contains('\0')).then_some(directory)
            }
        };
        let Some(directory) = path else { return };
        tab.files.osc_directory = true;
        tab.files.directory_probe_pending = false;
        self.set_terminal_directory(id, directory, cx);
    }

    fn set_terminal_directory(&mut self, id: u64, directory: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        let changed = tab.files.terminal_directory.as_ref() != Some(&directory);
        tab.files.terminal_directory = Some(directory.clone());
        let current = tab
            .files
            .navigation
            .as_deref()
            .unwrap_or(&tab.files.directory);
        if changed && !tab.files.follow_disabled && current != directory {
            self.list(id, directory, cx);
        }
    }

    pub(super) fn file_prompt_ready(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id).filter(|tab| tab.has_file_browser()) else {
            return;
        };
        if let Session::Local(shell) = tab.session {
            if tab.files.osc_directory {
                return;
            }
            let prompt = tab.terminal.read(cx).prompt_text();
            let directory = prompt.and_then(|line| match shell {
                LocalShell::Wsl => {
                    let wsl = tab.files.wsl.as_ref()?;
                    wsl.windows_directory(&prompt_directory(line, wsl.home())?)
                }
                _ => local::prompt_directory(shell, line),
            });
            if let Some(directory) = directory {
                self.set_terminal_directory(id, directory, cx);
            }
            return;
        }
        // A report for this prompt wins. A later prompt can still need a probe
        // after the shell stops emitting OSC 7.
        if std::mem::take(&mut tab.files.osc_directory) {
            return;
        }
        tab.files.directory_probe_pending = true;
        self.probe_terminal_directory(id, cx);
    }

    fn probe_terminal_directory(&mut self, id: u64, cx: &Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        if tab.files.directory_probe_active || !tab.files.directory_probe_pending {
            return;
        }
        let Some(connection) = tab.connection.clone() else {
            return;
        };
        tab.files.directory_probe_active = true;
        tab.files.directory_probe_pending = false;
        let terminal = tab.terminal.read(cx);
        let prompt = terminal.prompt_serial();
        let osc = terminal.cwd_serial();
        let operation_id = tab.operation_id;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let path = connection.terminal_directory().await;
            let _ = events
                .send(Event::File(
                    operation_id,
                    FileEvent::Directory { prompt, osc, path },
                ))
                .await;
        });
    }

    fn mutate_file(&mut self, id: u64, operation: RemoteMutation, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else { return };
        let Some(connection) = tab.connection.clone() else {
            return;
        };
        let operation_id = tab.operation_id;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = connection
                .mutate_remote(operation)
                .await
                .map_err(report_error);
            let _ = events
                .send(Event::File(operation_id, FileEvent::Mutation(result)))
                .await;
        });
        cx.notify();
    }

    fn open_entry(
        &mut self,
        id: u64,
        entry: RemoteEntry,
        explicit: bool,
        encoding: TextEncoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entry.is_dir {
            self.file_editor_to_activate = None;
            if explicit {
                self.list(id, entry.path, cx);
            } else {
                self.toggle_directory(id, entry, cx);
            }
            return;
        }
        if let Some(tab) = self.tab_mut(id) {
            tab.files.selected = Some(entry.clone());
        }
        if self.activate_open_editor(id, &entry.path, window, cx) {
            return;
        }
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(source) = FileSource::for_tab(tab) else {
            return;
        };
        if let Some(pending) = tab.files.pending_editors.get_mut(&entry.path)
            && pending.encoding == encoding
        {
            pending.open_external |= explicit;
            self.file_editor_to_activate = Some(pending.id);
            cx.notify();
            return;
        }
        let operation_id = tab.operation_id;
        let request = self.new_id();
        let path = entry.path.clone();
        let edit_limit_mb = self.settings.file_edit_limit_mb;
        let events = self.events.clone();
        let task = self
            .runtime
            .spawn(async move {
                let result = source
                    .open_text(&entry.path, encoding, edit_limit_mb)
                    .await
                    .map_err(report_error);
                let _ = events
                    .send(Event::File(
                        operation_id,
                        FileEvent::Text(request, entry, result),
                    ))
                    .await;
            })
            .abort_handle();
        if let Some(tab) = self.tab_mut(id) {
            tab.files.pending_editors.insert(
                path,
                PendingEditor {
                    id: request,
                    encoding,
                    open_external: explicit,
                    task,
                },
            );
        }
        self.file_editor_to_activate = Some(request);
        cx.notify();
    }

    fn open_system_entry(&mut self, entry: &RemoteEntry, cx: &mut Context<Self>) {
        if let Err(error) = open_local_file(Path::new(&entry.path)) {
            self.announce(format!("无法打开 {} · {error:#}", entry.name), cx);
        }
    }

    fn pick_upload(&mut self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let Some(directory) = self.upload_directory(id) else {
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择要上传的文件".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = prompt.await;
            let _ = view.update_in(cx, |this, _, cx| match result {
                Ok(Ok(Some(paths))) => this.upload(id, directory, paths, false, cx),
                Ok(Err(error)) => this.announce(format!("无法选择文件 · {error:#}"), cx),
                _ => {}
            });
        })
        .detach();
    }

    fn file_directory(&self, id: u64) -> Option<String> {
        let tab = self.tab(id)?;
        (tab.has_file_browser() && !tab.files.directory.is_empty())
            .then(|| tab.files.directory.clone())
    }

    fn insert_terminal_path(
        &mut self,
        id: u64,
        target: PathTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .active
            .filter(|active| *active == id)
            .and_then(|active| self.tab(active))
            .filter(|tab| tab.has_file_browser())
        else {
            return;
        };
        let Some(path) = tab.files.insertion_path(target) else {
            return;
        };
        let argument = terminal_path_argument(path, &tab.session, tab.files.wsl.as_ref());
        match argument {
            Ok(argument) => {
                tab.terminal
                    .update(cx, |terminal, cx| terminal.type_text(&argument, cx));
                self.select_tab(id, window, cx);
            }
            Err(message) => self.announce(message, cx),
        }
    }

    fn upload_directory(&self, id: u64) -> Option<String> {
        self.tab(id)?.connection.as_ref()?;
        self.file_directory(id)
    }

    pub(super) fn upload_terminal_clipboard(
        &mut self,
        id: u64,
        bytes: Vec<u8>,
        extension: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab(id) else { return };
        let Some(connection) = tab.connection.clone() else {
            self.announce("连接 SSH 会话后可上传剪贴板图片", cx);
            return;
        };
        let operation_id = tab.operation_id;
        let extension = match extension {
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "gif" => "gif",
            "webp" => "webp",
            "bmp" => "bmp",
            "tif" | "tiff" => "tiff",
            _ => "img",
        };
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let prepare = async {
                connection
                    .prepare_clipboard_directory(CLIPBOARD_DIRECTORY)
                    .await?;
                let directory = tempfile::Builder::new()
                    .prefix("capterm-clipboard-")
                    .tempdir()?;
                let path = directory
                    .path()
                    .join(format!("paste-{}.{extension}", uuid::Uuid::new_v4()));
                tokio::fs::write(&path, bytes).await?;
                Ok((directory, path))
            };
            let result = prepare.await.map_err(report_error);
            let _ = events
                .send(Event::File(operation_id, FileEvent::ClipboardReady(result)))
                .await;
        });
        self.announce("正在准备上传剪贴板图片…", cx);
    }

    fn upload(
        &mut self,
        id: u64,
        directory: String,
        paths: Vec<PathBuf>,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        let transfers = paths.into_iter().filter_map(|local| {
            let name = local.file_name()?.to_str()?.to_owned();
            Some((
                name,
                TransferSource::Upload {
                    local,
                    directory: directory.clone(),
                    insert,
                },
            ))
        });
        self.enqueue_transfers(id, transfers, cx);
    }

    fn pick_download(
        &mut self,
        id: u64,
        entry: RemoteEntry,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab(id).is_none_or(|tab| tab.connection.is_none()) {
            self.announce("SSH 会话已断开", cx);
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择下载目录".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = prompt.await;
            let _ = view.update_in(cx, |this, _, cx| match result {
                Ok(Ok(Some(directories))) => {
                    if let Some(directory) = directories.into_iter().next() {
                        this.download_entry(id, entry, directory, false, cx);
                    }
                }
                Ok(Err(error)) => this.announce(format!("无法选择下载位置 · {error:#}"), cx),
                _ => {}
            });
        })
        .detach();
    }

    fn download_to_default(&mut self, id: u64, entry: RemoteEntry, cx: &mut Context<Self>) {
        self.download_entry(id, entry, self.settings.download_dir(), false, cx);
    }

    fn download_entry(
        &mut self,
        id: u64,
        entry: RemoteEntry,
        directory: PathBuf,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        let name = if crate::ssh::valid_local_name(&entry.name) {
            &entry.name
        } else {
            "远端文件"
        };
        self.enqueue_transfers(
            id,
            [(
                entry.name.clone(),
                TransferSource::Download {
                    remote: entry.path,
                    local: directory.join(name),
                    open,
                },
            )],
            cx,
        );
    }

    fn download_and_open(&mut self, id: u64, entry: RemoteEntry, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        let directory = match tempfile::Builder::new()
            .prefix("capterm-preview-")
            .tempdir()
        {
            Ok(directory) => directory,
            Err(error) => {
                self.announce(format!("无法创建预览目录 · {error:#}"), cx);
                return;
            }
        };
        let local = directory.path().to_path_buf();
        tab.files.local_previews.push(directory);
        self.download_entry(id, entry, local, true, cx);
    }

    fn enqueue_transfers(
        &mut self,
        id: u64,
        jobs: impl IntoIterator<Item = (String, TransferSource)>,
        cx: &mut Context<Self>,
    ) {
        let batch = self.new_id();
        let options = TransferOptions {
            choice: match self.settings.transfer_conflict {
                TransferConflictPreference::Ask => TransferChoice::Ask,
                TransferConflictPreference::Overwrite => TransferChoice::Overwrite,
                TransferConflictPreference::KeepBoth => TransferChoice::KeepBoth,
                TransferConflictPreference::Skip => TransferChoice::Skip,
            },
            preserve_metadata: self.settings.transfer_preserve_metadata,
        };
        let transfers: Vec<_> = jobs
            .into_iter()
            .map(|(name, source)| Transfer {
                id: self.new_id(),
                batch,
                name,
                options: if matches!(source, TransferSource::Clipboard(_)) {
                    TransferOptions {
                        choice: TransferChoice::KeepBoth,
                        preserve_metadata: false,
                    }
                } else {
                    options
                },
                source,
                state: TransferState::Waiting,
                handle: None,
                copied: 0,
                total: 0,
                speed: 0.,
                measured: Instant::now(),
            })
            .collect();
        if transfers.is_empty() {
            return;
        }
        let Some(tab) = self.tab_mut(id).filter(|tab| tab.connection.is_some()) else {
            return;
        };
        tab.files.transfers.extend(transfers);
        tab.files.transfers_open = true;
        self.schedule_transfers(cx);
    }

    pub(super) fn schedule_transfers(&mut self, cx: &mut Context<Self>) {
        let limit = self.settings.transfer_concurrency.clamp(1, 16);
        let mut running = self
            .tabs
            .iter()
            .flat_map(|tab| &tab.files.transfers)
            .filter(|item| {
                matches!(
                    item.state,
                    TransferState::Running | TransferState::Cancelling
                )
            })
            .count();
        for tab in &mut self.tabs {
            let Some(connection) = &tab.connection else {
                continue;
            };
            for item in &mut tab.files.transfers {
                if running >= limit {
                    break;
                }
                if !matches!(item.state, TransferState::Waiting) {
                    continue;
                }
                running += 1;
                item.state = TransferState::Running;
                item.measured = Instant::now();
                if let Some(handle) = &item.handle {
                    let _ = handle.signal.send(TransferSignal::Running);
                    continue;
                }
                let (signal, watch) = watch::channel(TransferSignal::Running);
                let (operation_id, item_id) = (tab.operation_id, item.id);
                let source = item.source.clone();
                let options = item.options;
                let events = self.events.clone();
                let connection = connection.clone();
                let abort = self
                    .runtime
                    .spawn(async move {
                        let mut last = Instant::now() - Duration::from_secs(1);
                        let mut bytes_copied = 0;
                        let progress = |copied, total| {
                            bytes_copied = copied;
                            if last.elapsed() >= Duration::from_millis(150) || copied == total {
                                last = Instant::now();
                                let _ = events.try_send(Event::File(
                                    operation_id,
                                    FileEvent::Progress(item_id, copied, total),
                                ));
                            }
                        };
                        let result = match source {
                            TransferSource::Upload {
                                local, directory, ..
                            } => {
                                connection
                                    .upload_file(&local, &directory, options, watch, progress)
                                    .await
                            }
                            TransferSource::Clipboard(local) => {
                                connection
                                    .upload_file(
                                        &local,
                                        CLIPBOARD_DIRECTORY,
                                        options,
                                        watch,
                                        progress,
                                    )
                                    .await
                            }
                            TransferSource::Download { remote, local, .. } => {
                                connection
                                    .download_file(&remote, &local, options, watch, progress)
                                    .await
                            }
                        };
                        let _ = events
                            .send(Event::File(
                                operation_id,
                                FileEvent::Transfer(
                                    item_id,
                                    bytes_copied,
                                    result.map_err(report_error),
                                ),
                            ))
                            .await;
                    })
                    .abort_handle();
                item.handle = Some(TransferHandle { abort, signal });
            }
        }
        cx.notify();
    }

    fn control_transfer(
        &mut self,
        tab: u64,
        item: Option<u64>,
        cancel: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(tab) else { return };
        let resume_all =
            item.is_none()
                && !tab.files.transfers.iter().any(|item| {
                    matches!(item.state, TransferState::Waiting | TransferState::Running)
                });
        for transfer in tab
            .files
            .transfers
            .iter_mut()
            .filter(|transfer| item.is_none_or(|id| transfer.id == id))
        {
            if cancel && transfer.state.pending() {
                if let Some(handle) = &transfer.handle {
                    let _ = handle.signal.send(TransferSignal::Cancelled);
                    transfer.state = TransferState::Cancelling;
                } else {
                    transfer.state = TransferState::Cancelled;
                }
            } else if !cancel
                && matches!(
                    transfer.state,
                    TransferState::Running | TransferState::Waiting
                )
            {
                if let Some(handle) = &transfer.handle {
                    let _ = handle.signal.send(TransferSignal::Paused);
                }
                transfer.state = TransferState::Paused;
            } else if !cancel
                && (resume_all || item.is_some())
                && matches!(transfer.state, TransferState::Paused)
            {
                transfer.state = TransferState::Waiting;
            }
        }
        self.schedule_transfers(cx);
    }

    fn resolve_transfer(
        &mut self,
        tab: u64,
        id: u64,
        choice: TransferChoice,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(tab) else { return };
        let Some(batch) = tab
            .files
            .transfers
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.batch)
        else {
            return;
        };
        let apply_all = tab.files.conflict_all;
        if apply_all {
            tab.files.conflict_choices.insert(batch, choice);
        }
        for item in tab
            .files
            .transfers
            .iter_mut()
            .filter(|item| item.id == id || (apply_all && item.batch == batch))
        {
            if matches!(&item.state, TransferState::Conflict(conflict) if !(conflict.directory && choice == TransferChoice::Overwrite))
            {
                retry_transfer(item, choice);
            }
        }
        self.schedule_transfers(cx);
    }

    pub(super) fn cancel_file_transfers(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id) {
            for transfer in tab
                .files
                .transfers
                .iter_mut()
                .filter(|transfer| transfer.state.pending())
            {
                transfer.state = TransferState::Cancelled;
                transfer.handle = None;
            }
        }
        self.schedule_transfers(cx);
    }

    pub(super) fn file_transfer_count(&self) -> usize {
        self.tabs
            .iter()
            .flat_map(|tab| &tab.files.transfers)
            .filter(|item| item.state.pending())
            .count()
    }

    pub(super) fn show_file_transfers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending =
            |tab: &&super::Tab| tab.files.transfers.iter().any(|item| item.state.pending());
        let target = self
            .active_tab()
            .filter(pending)
            .or_else(|| self.tabs.iter().find(pending))
            .map(|tab| tab.id)
            .or(self.active);
        if let Some(id) = target.filter(|id| Some(*id) != self.active) {
            self.select_tab(id, window, cx);
        }
        if let Some(tab) = target.and_then(|id| self.tab_mut(id)) {
            tab.files.transfers_open = true;
            cx.notify();
        }
    }

    fn apply_transfer_result(
        &mut self,
        id: u64,
        transfer: u64,
        bytes_copied: u64,
        result: Result<TransferOutcome, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(item) = tab
            .files
            .transfers
            .iter_mut()
            .find(|item| item.id == transfer && item.state.pending())
        else {
            return;
        };
        item.handle = None;
        let source = item.source.clone();
        let directory = tab.files.directory.clone();
        let cancelling = matches!(item.state, TransferState::Cancelling);
        item.state = match result {
            Ok(TransferOutcome::Complete(path)) => TransferState::Complete(path),
            _ if cancelling => TransferState::Cancelled,
            Ok(TransferOutcome::Conflict(conflict)) => {
                if let Some(choice) = tab.files.conflict_choices.get(&item.batch).copied()
                    && choice != item.options.choice
                    && !(conflict.directory && choice == TransferChoice::Overwrite)
                {
                    retry_transfer(item, choice);
                    self.schedule_transfers(cx);
                    return;
                }
                TransferState::Conflict(conflict)
            }
            Ok(TransferOutcome::Cancelled) => TransferState::Cancelled,
            Ok(TransferOutcome::Skipped) => TransferState::Skipped,
            Err(error) => TransferState::Failed(error),
        };
        if let TransferState::Complete(path) = &item.state {
            let path = path.clone();
            item.copied = bytes_copied;
            item.total = bytes_copied;
            match source {
                TransferSource::Upload {
                    directory: target,
                    insert,
                    ..
                } => {
                    if insert {
                        self.append_file_path(id, &path, window, cx);
                    }
                    self.refresh_file_directory(id, directory, target, cx);
                }
                TransferSource::Clipboard(local) => {
                    self.remove_local_preview(id, &local);
                    if let Some(tab) = self.tab(id) {
                        tab.terminal
                            .update(cx, |terminal, cx| terminal.send_draft(&path, false, cx));
                    }
                    self.announce("已上传剪贴板图片并插入远端路径", cx);
                }
                TransferSource::Download { open: true, .. } => {
                    if let Err(error) = open_local_file(Path::new(&path)) {
                        self.announce(format!("无法打开已下载的文件 · {error:#}"), cx);
                    }
                }
                TransferSource::Download { .. } => {}
            }
        } else if !item.state.pending()
            && let TransferSource::Clipboard(local) = source
        {
            self.remove_local_preview(id, &local);
        }
        self.schedule_transfers(cx);
    }

    fn remove_local_preview(&mut self, id: u64, file: &Path) {
        if let Some(tab) = self.tab_mut(id) {
            tab.files
                .local_previews
                .retain(|directory| Some(directory.path()) != file.parent());
        }
    }

    fn refresh_file_directory(
        &mut self,
        id: u64,
        current: String,
        updated: String,
        cx: &mut Context<Self>,
    ) {
        if current == updated {
            self.list(id, current, cx);
            return;
        }
        let Some(tab) = self
            .tab(id)
            .filter(|tab| tab.files.branches.contains_key(&updated))
        else {
            return;
        };
        let Some(source) = FileSource::for_tab(tab) else {
            return;
        };
        let listing = tab.files.listing;
        let operation_id = tab.operation_id;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = source
                .list(&updated)
                .await
                .map(|(_, entries)| entries)
                .map_err(report_error);
            let _ = events
                .send(Event::File(
                    operation_id,
                    FileEvent::Branch(listing, updated, result),
                ))
                .await;
        });
    }

    fn append_file_path(&self, id: u64, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else { return };
        tab.draft.update(cx, |draft, cx| {
            let value = draft.value();
            let separator = if value.is_empty() || value.ends_with(char::is_whitespace) {
                ""
            } else {
                " "
            };
            draft.set_value(
                format!("{value}{separator}{}", shell_quote(path)),
                window,
                cx,
            );
        });
    }

    pub(super) fn apply_file_event(
        &mut self,
        operation_id: u64,
        event: FileEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let FileEvent::Saved(editor, edited, result) = event {
            self.finish_editor_save(editor, edited, result, window, cx);
            return;
        }
        let Some(tab) = self
            .tab_mut_for_operation(operation_id)
            .filter(|tab| tab.phase == Phase::Connected)
        else {
            return;
        };
        let id = tab.id;
        match event {
            FileEvent::List(request, result) if request == tab.files.listing => {
                tab.files.loading = false;
                let navigation = tab.files.navigation.take();
                match result {
                    Ok((directory, entries)) => {
                        if navigation.as_deref() == Some(".") {
                            tab.files.home.clone_from(&directory);
                        }
                        if tab.files.directory != directory {
                            tab.files.branches.clear();
                            tab.files.selected = None;
                        } else if let Some(selected) = &tab.files.selected
                            && file_parent(&selected.path, tab.is_local()).as_deref()
                                == Some(directory.as_str())
                        {
                            tab.files.selected = entries
                                .iter()
                                .find(|entry| entry.path == selected.path)
                                .cloned();
                        }
                        tab.files.directory = directory;
                        tab.files.entries = entries;
                        *tab.files.visible_rows.get_mut() = None;
                        tab.files.error = None;
                    }
                    Err(error) => tab.files.error = Some(error),
                }
            }
            FileEvent::Branch(listing, path, result) if listing == tab.files.listing => {
                if let Some(branch) = tab.files.branches.get_mut(&path) {
                    match result {
                        Ok(entries) => *branch = Some(entries),
                        Err(error) => {
                            tab.files.branches.remove(&path);
                            tab.files.error = Some(error);
                        }
                    }
                    *tab.files.visible_rows.get_mut() = None;
                }
            }
            FileEvent::Directory { prompt, osc, path } => {
                tab.files.directory_probe_active = false;
                let terminal = tab.terminal.read(cx);
                if prompt == terminal.prompt_serial()
                    && osc == terminal.cwd_serial()
                    && let Some(directory) =
                        path.or_else(|| prompt_directory(terminal.prompt_text()?, &tab.files.home))
                {
                    self.set_terminal_directory(id, directory, cx);
                }
                self.probe_terminal_directory(id, cx);
            }
            FileEvent::Text(request, entry, result) => {
                let std::collections::hash_map::Entry::Occupied(pending) =
                    tab.files.pending_editors.entry(entry.path.clone())
                else {
                    return;
                };
                if pending.get().id != request {
                    return;
                }
                let open_external = pending.remove().open_external;
                let local = tab.is_local();
                let activate =
                    self.active == Some(id) && self.file_editor_to_activate == Some(request);
                match result {
                    Ok(Some(text)) => {
                        self.install_file_editor(id, request, entry, text, window, cx)
                    }
                    Ok(None) if activate && open_external && local => {
                        self.open_system_entry(&entry, cx);
                    }
                    Ok(None) if activate && open_external => self.download_and_open(id, entry, cx),
                    Err(error) if self.active == Some(id) => {
                        self.announce(format!("无法打开 {} · {error}", entry.name), cx);
                    }
                    _ => {}
                }
                if self.file_editor_to_activate == Some(request) {
                    self.file_editor_to_activate = None;
                }
            }
            FileEvent::ClipboardReady(Ok((directory, path))) => {
                tab.files.local_previews.push(directory);
                self.enqueue_transfers(
                    id,
                    [("剪贴板图片".to_owned(), TransferSource::Clipboard(path))],
                    cx,
                );
            }
            FileEvent::ClipboardReady(Err(error)) => {
                self.announce(format!("无法上传剪贴板图片 · {error}"), cx)
            }
            FileEvent::Mutation(result) => {
                let directory = tab.files.directory.clone();
                match result {
                    Ok(()) => {
                        let open: Vec<_> = tab.files.branches.keys().cloned().collect();
                        self.list(id, directory.clone(), cx);
                        for target in open {
                            self.refresh_file_directory(id, directory.clone(), target, cx);
                        }
                    }
                    Err(error) => self.announce(format!("远端文件操作失败 · {error}"), cx),
                }
            }
            FileEvent::Progress(id, copied, total) => {
                if let Some(item) = tab
                    .files
                    .transfers
                    .iter_mut()
                    .find(|item| item.id == id && matches!(item.state, TransferState::Running))
                {
                    let elapsed = item.measured.elapsed().as_secs_f64();
                    if elapsed > 0. {
                        item.speed = copied.saturating_sub(item.copied) as f64 / elapsed;
                    }
                    item.copied = copied;
                    item.total = total;
                    item.measured = Instant::now();
                }
            }
            FileEvent::Transfer(transfer, bytes_copied, result) => {
                self.apply_transfer_result(id, transfer, bytes_copied, result, window, cx)
            }
            _ => {}
        }
        cx.notify();
    }
}

fn retry_transfer(item: &mut Transfer, choice: TransferChoice) {
    item.options.choice = choice;
    item.state = if choice == TransferChoice::Skip {
        TransferState::Skipped
    } else {
        TransferState::Waiting
    };
    item.copied = 0;
    item.speed = 0.;
}

fn open_local_file(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::PCWSTR;

        let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR::null(),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        anyhow::ensure!(
            result.0 as isize > 32,
            "系统无法打开该文件类型（{}）",
            result.0 as isize
        );
    }
    #[cfg(not(windows))]
    {
        let command = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(command).arg(path).spawn()?;
    }
    Ok(())
}

pub(super) fn parent_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((parent, _)) => parent.into(),
    }
}

fn file_parent(path: &str, local: bool) -> Option<String> {
    if local {
        Path::new(path).parent()?.to_str().map(str::to_owned)
    } else {
        (path != "/").then(|| parent_path(path))
    }
}

fn remote_path(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn linux_path_argument(path: &str) -> String {
    if path.starts_with('/')
        && path
            .chars()
            .all(|character| character.is_alphanumeric() || "/._-+@:".contains(character))
    {
        path.to_owned()
    } else {
        shell_quote(path)
    }
}

fn terminal_path_argument(
    path: &str,
    session: &Session,
    wsl: Option<&WslPaths>,
) -> Result<String, &'static str> {
    // Control characters can act as terminal keystrokes in unbracketed input.
    if path.chars().any(char::is_control) {
        return Err("路径包含控制字符，无法安全发送到终端");
    }
    match session {
        Session::Ssh(_) => Ok(linux_path_argument(path)),
        Session::Local(LocalShell::Wsl) => wsl
            .and_then(|wsl| wsl.linux_path(path))
            .filter(|path| !path.chars().any(char::is_control))
            .map(|path| linux_path_argument(&path))
            .ok_or("此路径无法映射到当前 WSL 发行版"),
        Session::Local(LocalShell::PowerShell) => {
            let mut quoted = String::with_capacity(path.len() + 2);
            quoted.push('\'');
            for character in path.chars() {
                if matches!(character, '\'' | '‘' | '’' | '‚' | '‛') {
                    quoted.push(character);
                }
                quoted.push(character);
            }
            quoted.push('\'');
            Ok(quoted)
        }
        Session::Local(LocalShell::CommandPrompt) => {
            // CMD can expand variables inside double quotes.
            if path.contains(['%', '!', '"']) {
                return Err("此路径包含 CMD 无法安全引用的字符");
            }
            // Native programs can interpret a backslash before the closing quote as an escape.
            let base = path.trim_end_matches('\\');
            let suffix = &path[base.len()..];
            Ok(format!("\"{base}\"{suffix}"))
        }
    }
}

fn prompt_directory(line: &str, home: &str) -> Option<String> {
    let line = line.trim().strip_suffix(['$', '#', '%'])?;
    let path = if line.starts_with(['/', '~']) {
        line.strip_suffix(' ')?
    } else {
        let (host, path) = line.split_once(':')?;
        if !host.contains('@') {
            return None;
        }
        path
    };
    if path.chars().any(char::is_control) {
        return None;
    }
    if path.starts_with('/') {
        return Some(path.to_owned());
    }
    if !home.is_empty() {
        let relative = path.strip_prefix('~')?;
        if relative.is_empty() || relative.starts_with('/') {
            return Some(format!("{home}{relative}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        FilePane, LocalShell, PathTarget, RemoteEntry, Session, linux_path_argument,
        prompt_directory, terminal_path_argument,
    };

    #[test]
    fn inserts_file_and_folder_paths_from_their_respective_targets() {
        let mut files = FilePane::default();
        assert_eq!(files.insertion_path(PathTarget::Folder), None);
        files.directory = "/srv/project".into();
        assert_eq!(files.insertion_path(PathTarget::File), None);
        assert_eq!(
            files.insertion_path(PathTarget::Folder),
            Some("/srv/project")
        );

        files.selected = Some(RemoteEntry {
            name: "notes.txt".into(),
            path: "/srv/project/docs/notes.txt".into(),
            is_dir: false,
            hidden: false,
            size: None,
            mtime: None,
            permissions: None,
        });
        assert_eq!(
            files.insertion_path(PathTarget::File),
            Some("/srv/project/docs/notes.txt")
        );
        assert_eq!(
            files.insertion_path(PathTarget::Folder),
            Some("/srv/project")
        );

        let selected = files.selected.as_mut().unwrap();
        selected.name = "docs".into();
        selected.path = "/srv/project/docs".into();
        selected.is_dir = true;
        assert_eq!(files.insertion_path(PathTarget::File), None);
        assert_eq!(
            files.insertion_path(PathTarget::Folder),
            Some("/srv/project/docs")
        );
    }

    #[test]
    fn extracts_full_paths_from_container_prompts() {
        assert_eq!(
            prompt_directory("/tmp/with spaces # ", "/root").as_deref(),
            Some("/tmp/with spaces")
        );
        assert_eq!(
            prompt_directory("root@container:~/data# ", "/root").as_deref(),
            Some("/root/data")
        );
        assert_eq!(prompt_directory("[root@container data]# ", "/root"), None);
        assert_eq!(prompt_directory("# ", "/root"), None);
    }

    #[test]
    fn quotes_file_paths_for_the_receiving_shell() {
        let powershell = Session::Local(LocalShell::PowerShell);
        let cmd = Session::Local(LocalShell::CommandPrompt);
        assert_eq!(
            terminal_path_argument(r"C:\A & B\O'Neil’s $file.txt", &powershell, None).unwrap(),
            r"'C:\A & B\O''Neil’’s $file.txt'"
        );
        assert_eq!(
            terminal_path_argument(r"C:\A & B\file.txt", &cmd, None).unwrap(),
            r#""C:\A & B\file.txt""#
        );
        assert_eq!(
            terminal_path_argument(r"C:\", &cmd, None).unwrap(),
            r#""C:"\"#
        );
        assert!(terminal_path_argument("C:\\unsafe\nfile", &powershell, None).is_err());
        assert!(terminal_path_argument(r"C:\%TEMP%\file", &cmd, None).is_err());
    }

    #[test]
    fn linux_file_paths_are_quoted_only_when_needed() {
        for path in [
            "/",
            "/home/user/project-1/src/main.rs",
            "/tmp/file+tag@1_0",
            "/root/项目/日志.txt",
        ] {
            assert_eq!(linux_path_argument(path), path);
        }
        assert_eq!(linux_path_argument("/tmp/two words"), "'/tmp/two words'");
        assert_eq!(linux_path_argument("/tmp/file[1]*"), "'/tmp/file[1]*'");
        assert_eq!(
            linux_path_argument("/tmp/O'Neil's $file"),
            r"'/tmp/O'\''Neil'\''s $file'"
        );
    }
}
