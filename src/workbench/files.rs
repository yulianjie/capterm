use std::path::PathBuf;

use gpui_kit::component::WindowExt;
use gpui_kit::*;

use super::dialogs::FormStatus;
use super::{Event, Phase, Workbench};
use crate::platform;
use crate::ssh::RemoteEntry;

#[derive(Default)]
pub(super) struct FilePane {
    pub(super) directory: String,
    pub(super) entries: Vec<RemoteEntry>,
    pub(super) selected: Option<String>,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    listing: u64,
    editor_request: u64,
}

pub(super) struct FileDrag {
    pub(super) tab: u64,
    pub(super) entry: RemoteEntry,
    pub(super) origin: Point<Pixels>,
    pub(super) started: bool,
}

pub(super) enum FileEvent {
    List(u64, Result<(String, Vec<RemoteEntry>), String>),
    Text(u64, RemoteEntry, Result<String, String>),
    Transfer(String, Option<String>),
}

fn report_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

impl Workbench {
    pub(super) fn list(&mut self, id: u64, directory: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(connection) = tab.connection.clone() else {
            return;
        };
        tab.files.listing += 1;
        tab.files.loading = true;
        tab.files.error = None;
        let listing = tab.files.listing;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = connection.list(&directory).await.map_err(report_error);
            let _ = events
                .send(Event::File(id, FileEvent::List(listing, result)))
                .await;
        });
        cx.notify();
    }

    pub(super) fn open_entry(&mut self, id: u64, entry: RemoteEntry, cx: &mut Context<Self>) {
        if entry.is_dir {
            self.list(id, entry.path, cx);
            return;
        }
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(connection) = tab.connection.clone() else {
            return;
        };
        tab.files.selected = Some(entry.path.clone());
        tab.files.editor_request += 1;
        let request = tab.files.editor_request;
        let notice = format!("Opening {}…", entry.name);
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = connection
                .read_text(&entry.path)
                .await
                .map_err(report_error);
            let _ = events
                .send(Event::File(id, FileEvent::Text(request, entry, result)))
                .await;
        });
        self.announce(notice, cx);
    }

    pub(super) fn save_entry(
        &mut self,
        id: u64,
        path: String,
        (original, edited): (String, String),
        status: std::rc::Rc<FormStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.tab(id).and_then(|tab| tab.connection.clone()) else {
            status.fail(
                "The SSH session closed before this file could be saved",
                window,
            );
            return;
        };
        let remote_path = path.clone();
        let operation = self.runtime.spawn(async move {
            connection
                .save_text(&remote_path, &original, &edited)
                .await
                .map_err(report_error)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = operation
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = view.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    window.close_dialog(cx);
                    let directory = this.tab(id).map(|tab| tab.files.directory.clone());
                    this.announce(format!("Saved to host · {path}"), cx);
                    if let Some(directory) = directory {
                        this.list(id, directory, cx);
                    }
                }
                Err(error) => status.fail(format!("Save failed · {error}"), window),
            });
        })
        .detach();
    }

    pub(super) fn pick_upload(&mut self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let Some(directory) = self.upload_directory(id) else {
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Select files to upload".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = prompt.await;
            let _ = view.update_in(cx, |this, _, cx| match result {
                Ok(Ok(Some(paths))) => this.upload(id, directory, paths, cx),
                Ok(Err(error)) => this.announce(format!("File picker failed: {error:#}"), cx),
                _ => {}
            });
        })
        .detach();
    }

    pub(super) fn upload_directory(&self, id: u64) -> Option<String> {
        let tab = self.tab(id)?;
        tab.connection.as_ref()?;
        (!tab.files.directory.is_empty() && tab.phase == Phase::Connected)
            .then(|| tab.files.directory.clone())
    }

    pub(super) fn upload(
        &mut self,
        id: u64,
        directory: String,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.tab(id).and_then(|tab| tab.connection.clone()) else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let events = self.events.clone();
        let count = paths.len();
        self.runtime.spawn(async move {
            let mut finished = 0;
            let mut failed = Vec::new();
            for path in paths {
                match connection.upload(&path, &directory).await {
                    Ok(()) => finished += 1,
                    Err(error) => failed.push(format!("{} · {error:#}", path.display())),
                }
            }
            let notice = if failed.is_empty() {
                format!("Uploaded {finished} file(s)")
            } else {
                format!("Uploaded {finished} of {count} · {}", failed.join(" · "))
            };
            let _ = events
                .send(Event::File(
                    id,
                    FileEvent::Transfer(notice, Some(directory)),
                ))
                .await;
        });
        self.announce(format!("Uploading {count} file(s)…"), cx);
    }

    pub(super) fn pick_download(
        &mut self,
        id: u64,
        entry: RemoteEntry,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab(id).is_none_or(|tab| tab.connection.is_none()) {
            self.announce("The SSH session is closed", cx);
            return;
        }
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let downloads = home.join("Downloads");
        let directory = if downloads.is_dir() { downloads } else { home };
        let prompt = cx.prompt_for_new_path(&directory, Some(&entry.name));
        cx.spawn_in(window, async move |view, cx| {
            let result = prompt.await;
            let _ = view.update_in(cx, |this, _, cx| match result {
                Ok(Ok(Some(path))) => this.download(id, entry, path, cx),
                Ok(Err(error)) => this.announce(format!("Save dialog failed: {error:#}"), cx),
                _ => {}
            });
        })
        .detach();
    }

    fn download(&mut self, id: u64, entry: RemoteEntry, path: PathBuf, cx: &mut Context<Self>) {
        let Some(connection) = self.tab(id).and_then(|tab| tab.connection.clone()) else {
            return;
        };
        let events = self.events.clone();
        let notice = format!("Downloading {}…", entry.name);
        self.runtime.spawn(async move {
            let notice = match connection.download(&entry.path, &path).await {
                Ok(()) => format!("Downloaded {}", path.display()),
                Err(error) => format!("Download failed · {} · {error:#}", entry.name),
            };
            let _ = events
                .send(Event::File(id, FileEvent::Transfer(notice, None)))
                .await;
        });
        self.announce(notice, cx);
    }

    pub(super) fn drag_entry(&self, id: u64, entry: RemoteEntry, cx: &mut Context<Self>) {
        if self.active != Some(id) || entry.is_dir || !platform::drag_supported() {
            return;
        }
        let Some(connection) = self.tab(id).and_then(|tab| tab.connection.clone()) else {
            return;
        };
        let runtime = self.runtime.handle().clone();
        let events = self.events.clone();
        // OLE can pump native messages during DoDragDrop. Run it after the
        // entity and window update has returned.
        cx.spawn(async move |view, cx| {
            let notifications = runtime.clone();
            let name = entry.name.clone();
            let result = platform::drag_file(
                &entry.name,
                entry.size,
                &runtime,
                move |path| async move { connection.download(&entry.path, &path).await },
                move |result| {
                    let notice = match result {
                        Ok(()) => format!("Downloaded {name}"),
                        Err(error) => format!("Download failed · {name} · {error:#}"),
                    };
                    notifications.spawn(async move {
                        let _ = events.send(Event::DragFinished(notice)).await;
                    });
                },
            );
            if let Err(error) = result {
                let _ = view.update(cx, |this, cx| {
                    this.announce(format!("Cannot drag {} · {error:#}", entry.name), cx);
                });
            }
        })
        .detach();
    }

    pub(super) fn apply_file_event(
        &mut self,
        id: u64,
        event: FileEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id).filter(|tab| tab.phase == Phase::Connected) else {
            return;
        };
        match event {
            FileEvent::List(request, result) if request == tab.files.listing => {
                tab.files.loading = false;
                match result {
                    Ok((directory, mut entries)) => {
                        entries.retain(|entry| entry.name != "." && entry.name != "..");
                        entries
                            .sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
                        tab.files.directory = directory;
                        tab.files.entries = entries;
                        tab.files.error = None;
                    }
                    Err(error) => tab.files.error = Some(error),
                }
                cx.notify();
            }
            FileEvent::Text(request, entry, result) if request == tab.files.editor_request => {
                if self.active != Some(id) {
                    return;
                }
                match result {
                    Ok(text) => {
                        self.notice.clear();
                        self.editor_dialog(id, entry, text, window, cx);
                    }
                    Err(error) => self.announce(
                        format!(
                            "Cannot edit {} · {error}. Download from the file menu.",
                            entry.name
                        ),
                        cx,
                    ),
                }
            }
            FileEvent::Transfer(notice, refresh) => {
                let refresh = refresh.filter(|directory| *directory == tab.files.directory);
                self.announce(notice, cx);
                if let Some(directory) = refresh {
                    self.list(id, directory, cx);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn parent_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((parent, _)) => parent.into(),
    }
}
