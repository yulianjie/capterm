use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::input::{Editor, EditorState, GutterDecoration, InputEvent};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, WindowExt};
use gpui_kit::prelude::*;
use similar::TextDiff;

use super::browser::{byte_size, file_icon, file_time, short_time, small_icon};
use super::*;
use crate::workbench::ui::{Palette, button, col, primary, row, tab_close_tool};

pub(in crate::workbench) struct FileEditor {
    pub(in crate::workbench) id: u64,
    terminal: u64,
    hostname: String,
    entry: RemoteEntry,
    state: Entity<EditorState>,
    original: RemoteText,
    pinned: bool,
    dirty: bool,
    busy: bool,
    cursor: (u32, u32),
    conflict: Option<EditorConflict>,
    error: Option<String>,
    diff: Option<Entity<DiffState>>,
    _change: Subscription,
    _cursor: Subscription,
}

struct EditorConflict {
    remote: Option<RemoteText>,
}

impl Workbench {
    pub(super) fn activate_open_editor(
        &mut self,
        terminal: u64,
        path: &str,
        pinned: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = self
            .file_editors
            .iter_mut()
            .find(|editor| editor.terminal == terminal && editor.entry.path == path)
        else {
            return false;
        };
        editor.pinned |= pinned;
        let id = editor.id;
        self.activate_file_editor(id, window, cx);
        true
    }

    pub(in crate::workbench) fn activate_file_editor(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.file_editors.iter().find(|editor| editor.id == id) else {
            return;
        };
        self.active_file_editor = Some(id);
        self.active = self.tab(editor.terminal).map(|tab| tab.id);
        self.set_monitored_tab(self.active);
        editor.state.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    pub(super) fn install_file_editor(
        &mut self,
        terminal: u64,
        entry: RemoteEntry,
        original: RemoteText,
        pinned: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hostname = self
            .tab(terminal)
            .map_or_else(String::new, |tab| tab.name().to_owned());
        let id = self.new_id();
        let language = editor_language(&entry.name).1;
        let normalized = normalize(&original.text).into_owned();
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .gutter_decorations_width(px(4.))
                .default_value(normalized)
        });
        let gutter = state.update(cx, |state, cx| {
            state.create_gutter_decorations_collection(Vec::new(), cx)
        });
        let change = cx.subscribe(&state, move |this, state, event, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = state.read(cx).value();
            let Some(editor) = this.file_editors.iter_mut().find(|editor| editor.id == id) else {
                return;
            };
            let original = normalize(&editor.original.text);
            editor.dirty = value.as_ref() != original.as_ref();
            if editor.dirty {
                editor.pinned = true;
            }
            let warn = Palette::new(this.settings.light).warn;
            gutter.set(changed_lines(original.as_ref(), &value, warn), cx);
            cx.notify();
        });
        let cursor = cx.observe(&state, move |this, state, cx| {
            let position = state.read(cx).cursor_position();
            let Some(editor) = this.file_editors.iter_mut().find(|editor| editor.id == id) else {
                return;
            };
            let cursor = (position.line, position.character);
            if editor.cursor != cursor {
                editor.cursor = cursor;
                cx.notify();
            }
        });
        let editor = FileEditor {
            id,
            terminal,
            hostname,
            entry,
            state,
            original,
            pinned,
            dirty: false,
            busy: false,
            cursor: (0, 0),
            conflict: None,
            error: None,
            diff: None,
            _change: change,
            _cursor: cursor,
        };
        if !pinned && let Some(preview) = self.file_editors.iter_mut().find(|editor| !editor.pinned)
        {
            *preview = editor;
        } else {
            self.file_editors.push(editor);
        }
        self.activate_file_editor(id, window, cx);
    }

    pub(in crate::workbench) fn close_file_editor(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.file_editors.iter().find(|editor| editor.id == id) else {
            return;
        };
        if editor.busy {
            return;
        }
        if !editor.dirty {
            self.remove_file_editor(id, window, cx);
            return;
        }
        let name = editor.entry.name.clone();
        let view = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .confirm()
                .title("放弃未保存的修改？")
                .description(format!("{name} 仍有未保存的修改。"))
                .ok_text("放弃修改并关闭")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("继续编辑")
                .on_ok(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.remove_file_editor(id, window, cx));
                    true
                })
        });
    }

    fn remove_file_editor(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.file_editors.retain(|editor| editor.id != id);
        if self.active_file_editor == Some(id) {
            self.active_file_editor = None;
            if let Some(terminal) = self.active {
                self.select_tab(terminal, window, cx);
            }
        }
        cx.notify();
    }

    fn active_editor(&self) -> Option<&FileEditor> {
        self.file_editors
            .iter()
            .find(|editor| Some(editor.id) == self.active_file_editor)
    }

    pub(super) fn is_file_dirty(&self, terminal: u64, path: &str) -> bool {
        self.file_editors
            .iter()
            .any(|editor| editor.terminal == terminal && editor.entry.path == path && editor.dirty)
    }

    pub(in crate::workbench) fn save_active_file_editor(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_file_editor {
            self.save_file_editor(id, false, cx);
        }
    }

    fn save_file_editor(&mut self, id: u64, force: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.file_editors.iter().find(|editor| editor.id == id) else {
            return;
        };
        if editor.original.readonly
            || editor.busy
            || !editor.dirty
            || (editor.conflict.is_some() && !force)
        {
            return;
        }
        let terminal = editor.terminal;
        let connection = self.tab(terminal).and_then(|tab| tab.connection.clone());
        let editor = self
            .file_editors
            .iter_mut()
            .find(|editor| editor.id == id)
            .unwrap();
        let Some(connection) = connection else {
            editor.error = Some("SSH 会话已断开，请恢复连接后重试。".into());
            cx.notify();
            return;
        };
        editor.busy = true;
        editor.error = None;
        let edited = editor.state.read(cx).value().to_string();
        let original = editor.original.clone();
        let path = editor.entry.path.clone();
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let result = connection
                .save_remote_text(&path, &original, &edited, force)
                .await
                .map_err(report_error);
            let _ = events
                .send(Event::File(terminal, FileEvent::Saved(id, edited, result)))
                .await;
        });
        cx.notify();
    }

    pub(super) fn finish_editor_save(
        &mut self,
        id: u64,
        edited: String,
        result: Result<SaveTextResult, String>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.file_editors.iter_mut().find(|editor| editor.id == id) else {
            return;
        };
        editor.busy = false;
        match result {
            Ok(SaveTextResult::Saved(original)) => {
                editor.entry.size = original.size;
                editor.entry.mtime = original.mtime;
                editor.original = original;
                editor.dirty = editor.state.read(cx).value().as_ref() != edited;
                editor.conflict = None;
                editor.diff = None;
                let path = editor.entry.path.clone();
                let terminal = editor.terminal;
                if let Some(current) = self.upload_directory(terminal) {
                    self.refresh_file_directory(terminal, current, parent_path(&path), cx);
                }
            }
            Ok(SaveTextResult::Conflict(remote)) => {
                editor.conflict = Some(EditorConflict { remote })
            }
            Err(error) => editor.error = Some(format!("保存到远端失败 · {error}")),
        }
        cx.notify();
    }

    fn reload_file_editor(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.file_editors.iter_mut().find(|editor| editor.id == id) else {
            return;
        };
        let Some(remote) = editor.conflict.take().and_then(|conflict| conflict.remote) else {
            return;
        };
        let text = normalize(&remote.text).into_owned();
        editor.original = remote;
        editor.dirty = false;
        editor.diff = None;
        editor.error = None;
        editor.state.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn toggle_file_diff(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(editor) = self.file_editors.iter_mut().find(|editor| editor.id == id) else {
            return;
        };
        if editor.diff.take().is_some() {
            cx.notify();
            return;
        }
        let Some(remote) = editor
            .conflict
            .as_ref()
            .and_then(|conflict| conflict.remote.as_ref())
        else {
            return;
        };
        let remote = normalize(&remote.text);
        let local = editor.state.read(cx).value();
        let diff = TextDiff::configure()
            .timeout(Duration::from_millis(200))
            .diff_lines(remote.as_ref(), local.as_ref());
        let patch = diff
            .unified_diff()
            .header("a/remote", "b/editor")
            .to_string();
        let files = DiffFile::parse(&patch).unwrap_or_default();
        editor.diff = Some(cx.new(|cx| DiffState::new(files, cx)));
        cx.notify();
    }

    pub(in crate::workbench) fn file_editor_status(
        &self,
        _: &App,
    ) -> Option<(String, &'static str, &'static str, &'static str, bool)> {
        let editor = self.active_editor()?;
        Some((
            format!("行 {}，列 {}", editor.cursor.0 + 1, editor.cursor.1 + 1),
            editor_language(&editor.entry.name).0,
            editor.original.encoding.label(),
            line_endings(&editor.original.text),
            editor.dirty,
        ))
    }

    pub(in crate::workbench) fn render_file_editor_tabs(
        &self,
        palette: Palette,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        self.file_editors
            .iter()
            .map(|editor| {
                let id = editor.id;
                let active = self.active_file_editor == Some(id);
                let label = editor.entry.path.clone();
                row()
                    .id(("editor-tab", id))
                    .relative()
                    .h(px(34.))
                    .max_w(px(192.))
                    .min_w(px(110.))
                    .flex_shrink_0()
                    .pl(px(10.))
                    .pr(px(4.))
                    .gap(px(5.))
                    .border_r_1()
                    .border_color(palette.line)
                    .bg(if active { palette.base } else { palette.app })
                    .text_color(if active { palette.ink } else { palette.muted })
                    .hover(|style| style.bg(palette.hover))
                    .cursor_pointer()
                    .tab_stop(true)
                    .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() > 1
                            && let Some(editor) =
                                this.file_editors.iter_mut().find(|editor| editor.id == id)
                        {
                            editor.pinned = true;
                        }
                        this.activate_file_editor(id, window, cx);
                    }))
                    .when(active, |tab| {
                        tab.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .w_full()
                                .h(px(2.))
                                .bg(palette.accent),
                        )
                    })
                    .child(small_icon(file_icon(&editor.entry), palette.muted))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_size(px(13.))
                            .when(!editor.pinned, |name| name.italic())
                            .when(editor.pinned, |name| name.font_weight(FontWeight::MEDIUM))
                            .child(editor.entry.name.clone()),
                    )
                    .child(
                        tab_close_tool(
                            ("editor-close", id),
                            if editor.dirty {
                                IconName::Circle
                            } else {
                                IconName::X
                            },
                            if editor.dirty {
                                "未保存，关闭编辑器"
                            } else {
                                "关闭编辑器"
                            },
                            palette,
                        )
                        .disabled(editor.busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_file_editor(id, window, cx);
                            },
                        )),
                    )
                    .into_any_element()
            })
            .collect()
    }

    pub(in crate::workbench) fn render_file_editor(
        &self,
        palette: Palette,
        mono: SharedString,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.active_editor()?;
        let id = editor.id;
        let terminal = editor.terminal;
        let entry = editor.entry.clone();
        let download = entry.clone();
        let connected = self
            .tab(terminal)
            .is_some_and(|tab| tab.connection.is_some());
        let readonly = editor.original.readonly;
        let download_button = button(("editor-download", id), "下载", palette)
            .h(px(24.))
            .flex_shrink_0()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .icon(small_icon(IconName::Download, palette.muted))
            .disabled(!connected)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.download_to_default(terminal, download.clone(), cx)
            }));
        let save = primary(
            ("editor-save", id),
            if editor.busy {
                "保存中…"
            } else {
                "保存"
            },
            palette,
        )
        .h(px(24.))
        .flex_shrink_0()
        .text_size(px(12.))
        .icon(small_icon(IconName::Save, palette.primary_text))
        .child(Kbd::new(Keystroke::parse("ctrl-s").unwrap()))
        .disabled(
            !connected || readonly || editor.busy || !editor.dirty || editor.conflict.is_some(),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.save_file_editor(id, false, cx)));
        let toolbar = row()
            .h(px(34.))
            .flex_shrink_0()
            .px(px(10.))
            .gap(px(8.))
            .border_b_1()
            .border_color(palette.line)
            .child(small_icon(file_icon(&entry), palette.muted))
            .child(
                row()
                    .flex_1()
                    .gap(px(6.))
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .child(entry.path.clone()),
                    )
                    .child(
                        row()
                            .gap(px(4.))
                            .px(px(4.))
                            .h(px(18.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(palette.line)
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .text_color(palette.muted)
                            .child(small_icon(IconName::Server, palette.muted))
                            .child(
                                div()
                                    .max_w(px(100.))
                                    .truncate()
                                    .child(editor.hostname.clone()),
                            ),
                    ),
            )
            .child(div().w(px(8.)))
            .child(
                row()
                    .gap(px(10.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .font_family(mono.clone())
                    .text_color(palette.muted)
                    .text_size(px(11.))
                    .child(editor_language(&entry.name).0)
                    .child(editor.original.encoding.label())
                    .child(line_endings(&editor.original.text))
                    .child("空格：2"),
            )
            .child(div().w(px(1.)).h(px(16.)).bg(palette.line))
            .child(download_button)
            .child(save);
        let panel = col()
            .flex_1()
            .size_full()
            .min_h_0()
            .bg(palette.base)
            .child(toolbar)
            .when(!connected, |panel| {
                panel.child(
                    row()
                        .min_h(px(32.))
                        .px(px(10.))
                        .gap(px(8.))
                        .flex_shrink_0()
                        .bg(palette.warn.opacity(0.12))
                        .child(small_icon(IconName::TriangleAlert, palette.warn))
                        .child("SSH 会话已断开，编辑内容保留在此标签。"),
                )
            });
        let panel = if let Some(conflict) = &editor.conflict {
            let changed = conflict.remote.as_ref().map_or_else(
                || "直接保存会覆盖远端的修改。".into(),
                |remote| {
                    let time = file_time(remote.mtime)
                        .map(|time| short_time(&time))
                        .unwrap_or_default();
                    match (editor.original.size, remote.size) {
                        (Some(before), Some(after)) => format!(
                            "{time} 远端文件被修改（{} → {}），直接保存会覆盖对方的修改。",
                            byte_size(before),
                            byte_size(after)
                        ),
                        _ => format!("{time} 远端文件被修改，直接保存会覆盖对方的修改。"),
                    }
                },
            );
            let compare = button(
                ("editor-compare", id),
                if editor.diff.is_some() {
                    "返回编辑"
                } else {
                    "比较差异"
                },
                palette,
            )
            .h(px(24.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .disabled(conflict.remote.is_none())
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_file_diff(id, cx)));
            let reload = button(("editor-reload", id), "重新加载", palette)
                .h(px(24.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .disabled(conflict.remote.is_none() || editor.busy)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.reload_file_editor(id, window, cx)),
                );
            let overwrite = button(("editor-overwrite", id), "仍然保存", palette)
                .h(px(24.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .disabled(editor.busy)
                .on_click(cx.listener(move |this, _, _, cx| this.save_file_editor(id, true, cx)));
            panel.child(
                row()
                    .min_h(px(39.))
                    .px(px(10.))
                    .gap(px(8.))
                    .flex_shrink_0()
                    .bg(palette.warn.opacity(0.12))
                    .border_b_1()
                    .border_color(palette.warn.opacity(0.5))
                    .child(small_icon(IconName::TriangleAlert, palette.warn))
                    .child(
                        row()
                            .flex_1()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("远端文件已更新。"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_color(palette.muted)
                                    .child(changed),
                            ),
                    )
                    .child(compare)
                    .child(reload)
                    .child(overwrite),
            )
        } else if readonly {
            panel.child(
                row()
                    .min_h(px(37.))
                    .px(px(10.))
                    .gap(px(8.))
                    .flex_shrink_0()
                    .bg(palette.tint)
                    .child(small_icon(IconName::Info, palette.accent))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .child("只读预览，显示文件的前 1 MB。"),
                    )
                    .child(
                        button(("editor-open-local", id), "下载后用本机程序打开", palette)
                            .h(px(24.))
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .disabled(!connected)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.download_and_open(terminal, entry.clone(), cx)
                            })),
                    ),
            )
        } else {
            panel
        };
        let panel = panel.children(editor.error.as_ref().map(|error| {
            row()
                .min_h(px(30.))
                .px(px(10.))
                .gap(px(8.))
                .bg(palette.danger.opacity(0.10))
                .text_color(palette.danger)
                .text_size(px(12.))
                .child(small_icon(IconName::TriangleAlert, palette.danger))
                .child(error.clone())
        }));
        let content = if let Some(diff) = &editor.diff {
            Diff::new(diff)
                .line_number(true)
                .size_full()
                .into_any_element()
        } else {
            Editor::new(&editor.state)
                .h(relative(1.))
                .w_full()
                .bordered(false)
                .appearance(false)
                .font_family(mono)
                .text_size(px(13.))
                .line_height(px(20.))
                .readonly(readonly)
                .aria_label(format!("编辑器：{}", editor.entry.name))
                .into_any_element()
        };
        Some(
            panel
                .child(div().flex_1().min_h_0().overflow_hidden().child(content))
                .into_any_element(),
        )
    }
}

fn normalize(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n").into()
    } else {
        text.into()
    }
}

fn changed_lines(original: &str, edited: &str, color: Hsla) -> Vec<GutterDecoration> {
    if original == edited {
        return Vec::new();
    }
    let prefix = original
        .split_inclusive('\n')
        .zip(edited.split_inclusive('\n'))
        .take_while(|(a, b)| a == b)
        .map(|(_, line)| line.len())
        .sum::<usize>();
    let suffix = original[prefix..]
        .split_inclusive('\n')
        .rev()
        .zip(edited[prefix..].split_inclusive('\n').rev())
        .take_while(|(a, b)| a == b)
        .map(|(_, line)| line.len())
        .sum::<usize>();
    let end = edited.len().saturating_sub(suffix);
    let mut offset = prefix.min(end);
    edited[offset..end]
        .split_inclusive('\n')
        .map(|line| {
            let range = offset..offset + line.len();
            offset = range.end;
            GutterDecoration::new(range).with_color(color)
        })
        .collect()
}

fn line_endings(text: &str) -> &'static str {
    if text.contains("\r\n") { "CRLF" } else { "LF" }
}

fn editor_language(name: &str) -> (&'static str, &'static str) {
    let name = name.to_ascii_lowercase();
    if name == "dockerfile" {
        return ("Dockerfile", "dockerfile");
    }
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("yaml" | "yml") => ("YAML", "yaml"),
        Some("toml") => ("TOML", "toml"),
        Some("json") => ("JSON", "json"),
        Some("rs") => ("Rust", "rust"),
        Some("py") => ("Python", "python"),
        Some("js") => ("JavaScript", "javascript"),
        Some("ts") => ("TypeScript", "typescript"),
        Some("sh" | "bash") => ("Shell", "bash"),
        Some("md") => ("Markdown", "markdown"),
        Some("xml") => ("XML", "xml"),
        Some("html") => ("HTML", "html"),
        Some("css") => ("CSS", "css"),
        Some("sql") => ("SQL", "sql"),
        Some("go") => ("Go", "go"),
        Some("conf") if name.starts_with("nginx") => ("Nginx", "text"),
        _ => ("纯文本", "text"),
    }
}
