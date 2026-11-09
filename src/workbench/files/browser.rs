use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Local};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::input::InputEvent;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Icon, WindowExt};
use gpui_kit::prelude::*;

use super::*;
use crate::platform;
use crate::terminal::TerminalCommand;
use crate::workbench::ui::{Palette, badge, button, col, icon_tool, input, menu_action, row};

const SIZE_WIDTH: Pixels = px(58.);
const TIME_WIDTH: Pixels = px(39.);

enum NameAction {
    File(String),
    Directory(String),
    Rename(RemoteEntry),
    Chmod(RemoteEntry),
}

impl NameAction {
    fn labels(&self) -> (&'static str, &'static str) {
        match self {
            Self::File(_) => ("新建文件", "文件名"),
            Self::Directory(_) => ("新建文件夹", "文件夹名"),
            Self::Rename(_) => ("重命名", "新名称"),
            Self::Chmod(_) => ("修改权限", "八进制权限，例如 0644"),
        }
    }

    fn initial(&self) -> String {
        match self {
            Self::Rename(entry) => entry.name.clone(),
            Self::Chmod(entry) => entry
                .permissions
                .map(|mode| format!("{:04o}", mode & 0o7777))
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn operation(&self, value: &str) -> Result<RemoteMutation, &'static str> {
        if let Self::Chmod(entry) = self {
            let mode = u32::from_str_radix(value.trim(), 8)
                .ok()
                .filter(|mode| *mode <= 0o7777)
                .ok_or("请输入 0000 至 7777 之间的八进制权限")?;
            return Ok(RemoteMutation::Chmod {
                path: entry.path.clone(),
                mode,
            });
        }
        if value.trim().is_empty() || matches!(value, "." | "..") || value.contains(['/', '\0']) {
            return Err("请输入有效的名称，名称不能包含斜杠");
        }
        Ok(match self {
            Self::File(directory) => RemoteMutation::CreateFile(remote_path(directory, value)),
            Self::Directory(directory) => {
                RemoteMutation::CreateDirectory(remote_path(directory, value))
            }
            Self::Rename(entry) => RemoteMutation::Rename {
                old: entry.path.clone(),
                new: remote_path(&parent_path(&entry.path), value),
            },
            Self::Chmod(_) => unreachable!(),
        })
    }
}

pub(super) fn file_icon(entry: &RemoteEntry) -> IconName {
    let name = entry.name.to_ascii_lowercase();
    if name.starts_with(".env") {
        return IconName::FileKey;
    }
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension {
        "zip" | "gz" | "tgz" | "xz" | "tar" | "7z" => IconName::FileArchive,
        "txt" | "log" | "md" => IconName::FileText,
        "yaml" | "yml" | "toml" | "json" | "conf" | "sh" | "py" | "rs" | "js" => IconName::FileCode,
        _ if matches!(name.as_str(), "makefile" | "dockerfile") => IconName::FileCode,
        _ if name == "version" => IconName::FileText,
        _ => IconName::File,
    }
}

pub(super) fn small_icon(name: IconName, color: Hsla) -> Icon {
    Icon::new(name).size(px(14.)).text_color(color)
}

pub(super) fn file_tool(
    id: impl Into<ElementId>,
    icon: IconName,
    title: &'static str,
    palette: Palette,
) -> Button {
    icon_tool(id, icon, title, palette)
}

pub(super) fn byte_size(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    for unit in ["KB", "MB", "GB", "TB"] {
        value /= 1000.;
        if value < 1000. || unit == "TB" {
            return format!("{value:.1} {unit}");
        }
    }
    unreachable!()
}

pub(super) fn file_time(timestamp: Option<u32>) -> Option<DateTime<Local>> {
    DateTime::from_timestamp(i64::from(timestamp?), 0).map(|time| time.with_timezone(&Local))
}

pub(super) fn short_time(time: &DateTime<Local>) -> String {
    let format = if time.date_naive() == Local::now().date_naive() {
        "%H:%M"
    } else {
        "%m-%d"
    };
    time.format(format).to_string()
}

impl Workbench {
    pub(in crate::workbench) fn render_files(
        &mut self,
        width: Pixels,
        palette: Palette,
        mono: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self.active_tab();
        let local = tab.is_some_and(Tab::is_local);
        let active = tab.filter(|tab| tab.has_file_browser()).map(|tab| tab.id);
        let refresh = file_tool("file-refresh", IconName::RefreshCw, "刷新文件", palette)
            .disabled(active.is_none())
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(id) = active {
                    this.refresh_files(id, cx);
                }
            }));
        let view = cx.weak_entity();
        let more = file_tool("file-more", IconName::Ellipsis, "更多文件操作", palette)
            .disabled(active.is_none())
            .dropdown_menu(move |menu, _, _| {
                let Some(id) = active else {
                    return menu;
                };
                menu.item(menu_action(
                    "复制当前路径",
                    &view,
                    move |this, _, cx| {
                        if let Some(path) = this.file_directory(id) {
                            cx.write_to_clipboard(ClipboardItem::new_string(path));
                        }
                    },
                ))
                .item(menu_action("刷新", &view, move |this, _, cx| {
                    this.refresh_files(id, cx)
                }))
            });
        let heading = row()
            .h(px(34.))
            .flex_shrink_0()
            .px(px(10.))
            .gap(px(7.))
            .border_b_1()
            .border_color(palette.line)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("文件"))
            .child(badge(
                if local { "本地" } else { "SFTP" },
                palette,
                mono.clone(),
            ))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .font_family(mono.clone())
                    .text_color(palette.muted)
                    .text_size(px(12.))
                    .child(tab.map_or_else(String::new, |tab| tab.name().to_owned())),
            )
            .child(refresh)
            .child(more);
        let pane = col()
            .w(width)
            .h_full()
            .relative()
            .flex_shrink_0()
            .bg(palette.panel)
            .border_r_1()
            .border_color(palette.line)
            .child(heading);
        let Some(tab) = tab.filter(|tab| tab.has_file_browser()) else {
            let connecting = tab.filter(|tab| tab.phase == Phase::Connecting);
            return pane
                .child(
                    col()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .px(px(16.))
                        .gap(px(9.))
                        .text_center()
                        .child(small_icon(
                            if connecting.is_some() {
                                IconName::LoaderCircle
                            } else {
                                IconName::Folder
                            },
                            palette.dim,
                        ))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(
                            connecting.map_or_else(
                                || "还没有可浏览的文件".into(),
                                |tab| format!("{} {}", tab.phase.label(tab.is_local()), tab.name()),
                            ),
                        ))
                        .child(div().text_size(px(12.)).text_color(palette.muted).child(
                            match connecting.map(Tab::is_local) {
                                Some(true) => "终端启动后会自动打开初始目录。",
                                Some(false) => "连接建立后会自动打开登录目录。",
                                None => "连接 SSH 或打开本地终端后，这里显示会话文件。",
                            },
                        )),
                )
                .into_any_element();
        };
        let id = tab.id;
        let files = &tab.files;
        let directory = files.directory.clone();
        let can_operate = !directory.is_empty();
        let crumb_values = breadcrumbs(&directory, width, local);
        let separator = if local { "\\" } else { "/" };
        let pathbar = row()
            .h(px(32.))
            .flex_shrink_0()
            .px(px(7.))
            .gap(px(4.))
            .border_b_1()
            .border_color(palette.line)
            .child(
                file_tool("file-home", IconName::House, "主目录", palette).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.list(id, ".".into(), cx);
                    },
                )),
            )
            .child(
                row()
                    .flex_1()
                    .font_family(mono.clone())
                    .text_size(px(12.))
                    .gap(px(3.))
                    .overflow_hidden()
                    .text_color(palette.muted)
                    .when(!local, |path| path.child("/"))
                    .children(
                        crumb_values
                            .into_iter()
                            .enumerate()
                            .map(|(i, (name, path))| {
                                let title = path.clone();
                                row()
                                    .min_w_0()
                                    .gap(px(3.))
                                    .when(i > 0, |crumb| crumb.child(separator))
                                    .child(
                                        div()
                                            .id(("file-crumb", i))
                                            .truncate()
                                            .cursor_pointer()
                                            .tab_stop(true)
                                            .hover(|style| style.text_color(palette.accent))
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(title.clone()).build(window, cx)
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.list(id, path.clone(), cx)
                                            }))
                                            .child(name),
                                    )
                            }),
                    ),
            )
            .child(
                Button::new("file-follow")
                    .ghost()
                    .label("跟随终端")
                    .tooltip("跟随终端当前目录（OSC 7 或 Shell 提示符）")
                    .h(px(23.))
                    .px(px(5.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if !files.follow_disabled {
                        palette.accent
                    } else {
                        palette.muted
                    })
                    .when(!files.follow_disabled, |button| button.bg(palette.tint))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(tab) = this.tab_mut(id) else {
                            return;
                        };
                        tab.files.follow_disabled = !tab.files.follow_disabled;
                        if !tab.files.follow_disabled {
                            if let Some(path) = tab.files.terminal_directory.clone() {
                                this.list(id, path, cx);
                            } else {
                                this.file_prompt_ready(id, cx);
                            }
                        }
                        cx.notify();
                    })),
            );
        let parent = file_parent(&directory, local);
        let selected = files.selected.clone().filter(|entry| !entry.is_dir);
        let toolbar = row()
            .h(px(32.))
            .flex_shrink_0()
            .gap(px(1.))
            .px(px(5.))
            .border_b_1()
            .border_color(palette.line)
            .child(
                file_tool("file-parent", IconName::ArrowUp, "上一级", palette)
                    .disabled(!can_operate || parent.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(path) = &parent {
                            this.list(id, path.clone(), cx);
                        }
                    })),
            )
            .when(local, |toolbar| {
                toolbar.child(
                    file_tool(
                        "file-pick-folder",
                        IconName::FolderOpen,
                        "选择本地文件夹…",
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_local_directory(id, window, cx);
                    })),
                )
            })
            .when(!local, |toolbar| {
                toolbar
                    .child(
                        file_tool("file-new", IconName::FilePlus, "新建文件", palette)
                            .disabled(!can_operate)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(path) = this.upload_directory(id) {
                                    this.file_name_dialog(id, NameAction::File(path), window, cx);
                                }
                            })),
                    )
                    .child(
                        file_tool("file-mkdir", IconName::FolderPlus, "新建文件夹", palette)
                            .disabled(!can_operate)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(path) = this.upload_directory(id) {
                                    this.file_name_dialog(
                                        id,
                                        NameAction::Directory(path),
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        file_tool("file-upload", IconName::Upload, "上传文件…", palette)
                            .disabled(!can_operate)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_upload(id, window, cx)
                            })),
                    )
                    .child(
                        file_tool("file-download", IconName::Download, "下载所选文件", palette)
                            .disabled(selected.is_none())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(entry) = selected.clone() {
                                    this.download_to_default(id, entry, cx);
                                }
                            })),
                    )
            })
            .child(
                file_tool(
                    "file-send-path",
                    IconName::Terminal,
                    "发送路径到终端（优先使用选中项）",
                    palette,
                )
                .disabled(!can_operate)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.send_file_path(id, window, cx);
                })),
            )
            .child(div().flex_1())
            .child(
                file_tool(
                    "file-hidden",
                    if !files.hide_hidden {
                        IconName::Eye
                    } else {
                        IconName::EyeOff
                    },
                    "显示隐藏文件",
                    palette,
                )
                .when(!files.hide_hidden, |button| {
                    button.bg(palette.tint).text_color(palette.accent)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(tab) = this.tab_mut(id) {
                        tab.files.hide_hidden = !tab.files.hide_hidden;
                    }
                    cx.notify();
                })),
            )
            .child(
                file_tool(
                    "file-filter-toggle",
                    IconName::ListFilter,
                    "筛选文件",
                    palette,
                )
                .when(files.filter_open, |button| {
                    button.bg(palette.tint).text_color(palette.accent)
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| this.toggle_file_filter(id, window, cx)),
                ),
            );
        let columns = row()
            .h(px(24.))
            .flex_shrink_0()
            .px(px(10.))
            .gap(px(5.))
            .border_b_1()
            .border_color(palette.line)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(palette.muted)
            .child(div().flex_1().child("名称"))
            .child(
                div()
                    .w(SIZE_WIDTH)
                    .flex_shrink_0()
                    .text_right()
                    .child("大小"),
            )
            .child(
                div()
                    .w(TIME_WIDTH)
                    .flex_shrink_0()
                    .text_right()
                    .child("修改"),
            );
        let row_count = files.visible_row_count(cx);
        let row_font = mono.clone();
        let rows = uniform_list(
            ("file-list", id),
            row_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let Some(tab) = this.tab(id) else {
                    return Vec::new();
                };
                let visible = tab.files.visible_rows.borrow();
                range
                    .filter_map(|index| {
                        let (entry, depth) = if let Some(visible) = visible.as_ref() {
                            visible.entry(&tab.files, index)?
                        } else {
                            (tab.files.entries.get(index)?, 0)
                        };
                        Some(this.render_file_row(tab, entry, depth, palette, &row_font, cx))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .with_sizing_behavior(ListSizingBehavior::Auto)
        .flex_1()
        .w_full()
        .min_h_0();
        let scroll = col()
            .id(("file-rows", id))
            .flex_1()
            .min_h_0()
            .pt(px(3.))
            .overflow_hidden()
            .when(files.loading && files.entries.is_empty(), |list| {
                list.child(file_message("正在读取目录…", palette.muted))
            })
            .when(
                !files.loading && files.entries.is_empty() && files.error.is_none(),
                |list| list.child(file_message("该目录为空。", palette.muted)),
            )
            .when_some(files.error.as_ref(), |list, error| {
                list.child(file_message(
                    format!("无法读取目录 · {error}"),
                    palette.warn,
                ))
            })
            .child(rows)
            .when(!local && can_operate, |list| {
                list.on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let count = event.drag(cx).paths().len();
                        this.update_drop_target(
                            id,
                            event.event.position,
                            &directory,
                            count,
                            event.bounds.contains(&event.event.position),
                            false,
                            cx,
                        );
                    },
                ))
                .on_drop(cx.listener(
                    move |this, paths: &ExternalPaths, _, cx| {
                        if let Some(path) = this.upload_directory(id) {
                            this.clear_drop_target(id);
                            this.upload(id, path, paths.paths().to_vec(), false, cx);
                        }
                    },
                ))
            });
        let filter = files
            .filter
            .as_ref()
            .filter(|_| files.filter_open)
            .map(|(state, _)| {
                div()
                    .px(px(7.))
                    .py(px(5.))
                    .child(input(state, "筛选文件", palette).h(px(26.)))
            });
        let drop_hint = files
            .drop_hover
            .as_ref()
            .filter(|_| cx.has_active_drag())
            .map(|(_, path, count)| {
                row()
                    .mx(px(8.))
                    .mb(px(8.))
                    .p(px(9.))
                    .gap(px(8.))
                    .flex_shrink_0()
                    .rounded(px(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(palette.accent)
                    .bg(palette.tint)
                    .child(
                        div()
                            .p(px(6.))
                            .rounded(px(5.))
                            .bg(palette.accent)
                            .child(small_icon(IconName::Upload, palette.primary_text)),
                    )
                    .child(
                        col()
                            .flex_1()
                            .gap(px(4.))
                            .text_size(px(12.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("松开以上传 {count} 个文件到")),
                            )
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .truncate()
                                    .child(path.clone()),
                            )
                            .child(
                                div()
                                    .text_color(palette.muted)
                                    .child("拖到空白处可上传到当前目录。"),
                            ),
                    )
            });
        pane.child(pathbar)
            .child(toolbar)
            .children(filter)
            .child(columns)
            .child(scroll)
            .children(drop_hint)
            .children(self.render_transfers(id, files, palette, window, cx))
            .when(!local && platform::drag_supported(), |pane| {
                pane.child(self.track_native_file_drag(id, cx))
            })
            .into_any_element()
    }

    pub(in crate::workbench) fn refresh_files(&mut self, id: u64, cx: &mut Context<Self>) {
        let directory = self.file_directory(id).unwrap_or_else(|| ".".into());
        let branches: Vec<_> = self
            .tab(id)
            .into_iter()
            .flat_map(|tab| tab.files.branches.keys().cloned())
            .collect();
        self.list(id, directory.clone(), cx);
        for path in branches {
            self.refresh_file_directory(id, directory.clone(), path, cx);
        }
    }

    fn pick_local_directory(&mut self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择本地文件夹".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = prompt.await;
            let _ = view.update_in(cx, |this, _, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.first() {
                        if let Some(path) = path.to_str() {
                            this.list(id, path.into(), cx);
                        } else {
                            this.announce("无法读取该文件夹路径", cx);
                        }
                    }
                }
                Ok(Err(error)) => this.announce(format!("无法选择文件夹 · {error:#}"), cx),
                _ => {}
            });
        })
        .detach();
    }

    fn toggle_file_filter(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        tab.files.filter_open = !tab.files.filter_open;
        if tab.files.filter_open {
            let (state, _) = tab.files.filter.get_or_insert_with(|| {
                let state = cx.new(|cx| InputState::new(window, cx).placeholder("筛选名称…"));
                let subscription = cx.subscribe(&state, |_, _, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                });
                (state, subscription)
            });
            state.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn render_file_row(
        &self,
        tab: &Tab,
        entry: &RemoteEntry,
        depth: usize,
        palette: Palette,
        mono: &SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = tab.id;
        let files = &tab.files;
        let local = tab.is_local();
        let open = files.branches.contains_key(&entry.path);
        let selected = files
            .selected
            .as_ref()
            .is_some_and(|selection| selection.path == entry.path);
        let icon = if entry.is_dir {
            if open {
                IconName::FolderOpen
            } else {
                IconName::Folder
            }
        } else {
            file_icon(entry)
        };
        let clicked = entry.clone();
        let title = entry.path.clone();
        let time = file_time(entry.mtime);
        let full_time = time
            .as_ref()
            .map(|time| time.format("%Y-%m-%d %H:%M:%S %Z").to_string());
        let file_row = row()
            .id(SharedString::from(format!("remote-{}", entry.path)))
            .w_full()
            .h(px(26.))
            .gap(px(5.))
            .pr(px(10.))
            .pl(px(5. + depth.min(15) as f32 * 14.))
            .text_size(px(13.))
            .cursor_pointer()
            .tab_stop(true)
            .hover(|style| style.bg(palette.hover))
            .focus_visible(|style| style.bg(palette.hover))
            .when(selected, |row| row.bg(palette.tint))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.open_entry(
                    id,
                    clicked.clone(),
                    event.click_count() > 1,
                    TextEncoding::Utf8,
                    window,
                    cx,
                );
            }))
            .child(
                div()
                    .w(px(10.))
                    .flex_shrink_0()
                    .when(entry.is_dir, |arrow| {
                        arrow.child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(px(10.))
                            .text_color(palette.dim),
                        )
                    }),
            )
            .child(small_icon(
                icon,
                if entry.is_dir {
                    palette.accent
                } else {
                    palette.muted
                },
            ))
            .child(
                row()
                    .flex_1()
                    .gap(px(5.))
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if entry.hidden {
                                palette.muted
                            } else {
                                palette.ink
                            })
                            .child(entry.name.clone()),
                    )
                    .when(self.is_file_dirty(id, &entry.path), |name| {
                        name.child(
                            div()
                                .size(px(5.))
                                .rounded_full()
                                .bg(palette.warn)
                                .flex_shrink_0(),
                        )
                    }),
            )
            .child(
                div()
                    .w(SIZE_WIDTH)
                    .flex_shrink_0()
                    .text_right()
                    .font_family(mono.clone())
                    .text_size(px(12.))
                    .text_color(palette.muted)
                    .truncate()
                    .child(
                        entry
                            .size
                            .filter(|_| !entry.is_dir)
                            .map(byte_size)
                            .unwrap_or_default(),
                    ),
            )
            .child(
                div()
                    .id("mtime")
                    .w(TIME_WIDTH)
                    .flex_shrink_0()
                    .text_right()
                    .font_family(mono.clone())
                    .text_size(px(12.))
                    .text_color(palette.muted)
                    .truncate()
                    .when_some(full_time, |cell, full_time| {
                        cell.tooltip(move |window, cx| {
                            Tooltip::new(format!("修改于 {full_time}")).build(window, cx)
                        })
                    })
                    .child(time.as_ref().map(short_time).unwrap_or_default()),
            )
            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx));
        let drag = FileDrag {
            tab: id,
            entry: entry.clone(),
        };
        let view = cx.weak_entity();
        let file_row = file_row.when(!local, |row| {
            row.on_drag(drag, move |drag, _, _, cx| {
                if !drag.entry.is_dir && platform::drag_supported() {
                    let _ = view.update(cx, |this, _| {
                        if let Some(tab) = this.tab_mut(drag.tab) {
                            tab.files.native_drag = Some(drag.entry.clone());
                        }
                    });
                }
                cx.new(|_| FileDragPreview(drag.entry.name.clone(), palette))
            })
        });
        let destination = entry.path.clone();
        let drop_target = destination.clone();
        let file_row = file_row.when(!local && entry.is_dir, |row| {
            row.drag_over::<ExternalPaths>(move |style, _, _, _| {
                style
                    .bg(palette.tint)
                    .border_1()
                    .border_color(palette.accent)
            })
            .on_drag_move::<ExternalPaths>(cx.listener(
                move |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        let count = event.drag(cx).paths().len();
                        this.update_drop_target(
                            id,
                            event.event.position,
                            &drop_target,
                            count,
                            true,
                            true,
                            cx,
                        );
                    }
                },
            ))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                cx.stop_propagation();
                this.clear_drop_target(id);
                this.upload(id, destination.clone(), paths.paths().to_vec(), false, cx);
            }))
        });
        let view = cx.weak_entity();
        let entry = entry.clone();
        file_row
            .context_menu(move |menu, _, _| {
                let open = entry.clone();
                if local {
                    let path = entry.path.clone();
                    let menu = menu.item(menu_action(
                        if entry.is_dir {
                            "打开文件夹"
                        } else {
                            "打开 / 编辑"
                        },
                        &view,
                        move |this, window, cx| {
                            this.open_entry(id, open.clone(), true, TextEncoding::Utf8, window, cx);
                        },
                    ));
                    let menu = if entry.is_dir {
                        menu
                    } else {
                        let gbk = entry.clone();
                        let external = entry.clone();
                        menu.item(menu_action(
                            "用 GBK 编码打开",
                            &view,
                            move |this, window, cx| {
                                this.open_entry(
                                    id,
                                    gbk.clone(),
                                    true,
                                    TextEncoding::Gbk,
                                    window,
                                    cx,
                                );
                            },
                        ))
                        .item(menu_action(
                            "使用系统程序打开",
                            &view,
                            move |this, _, cx| {
                                this.open_system_entry(&external, cx);
                            },
                        ))
                    };
                    return menu.item(menu_action("复制路径", &view, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))
                    }));
                }
                let download = entry.clone();
                let gbk = entry.clone();
                let path = entry.path.clone();
                let directory = if entry.is_dir {
                    entry.path.clone()
                } else {
                    parent_path(&entry.path)
                };
                let cd = directory.clone();
                let new = directory.clone();
                let rename = entry.clone();
                let chmod = entry.clone();
                let delete = entry.clone();
                let menu = menu.item(menu_action(
                    if entry.is_dir {
                        "打开文件夹"
                    } else {
                        "打开 / 编辑"
                    },
                    &view,
                    move |this, window, cx| {
                        this.open_entry(id, open.clone(), true, TextEncoding::Utf8, window, cx);
                    },
                ));
                let menu = if entry.is_dir {
                    menu
                } else {
                    menu.item(menu_action(
                        "用 GBK 编码打开",
                        &view,
                        move |this, window, cx| {
                            this.open_entry(id, gbk.clone(), true, TextEncoding::Gbk, window, cx)
                        },
                    ))
                    .item(menu_action(
                        "下载到…",
                        &view,
                        move |this, window, cx| {
                            this.pick_download(id, download.clone(), window, cx)
                        },
                    ))
                };
                menu.item(menu_action("复制路径", &view, move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))
                }))
                .item(menu_action(
                    "在终端中 cd 到此处",
                    &view,
                    move |this, _, cx| {
                        if let Some(tab) = this.tab(id) {
                            tab.link.forward(TerminalCommand::Data(
                                format!("cd -- {}\r", shell_quote(&cd)).into_bytes(),
                            ));
                        }
                        this.active_file_editor = None;
                        cx.notify();
                    },
                ))
                .separator()
                .item(menu_action(
                    "重命名…",
                    &view,
                    move |this, window, cx| {
                        this.file_name_dialog(id, NameAction::Rename(rename.clone()), window, cx)
                    },
                ))
                .item(menu_action(
                    "修改权限…",
                    &view,
                    move |this, window, cx| {
                        this.file_name_dialog(id, NameAction::Chmod(chmod.clone()), window, cx)
                    },
                ))
                .item(menu_action("删除…", &view, move |this, window, cx| {
                    this.confirm_delete(id, delete.clone(), window, cx)
                }))
                .separator()
                .item(menu_action(
                    "新建文件…",
                    &view,
                    move |this, window, cx| {
                        this.file_name_dialog(id, NameAction::File(new.clone()), window, cx)
                    },
                ))
                .item(menu_action(
                    "新建文件夹…",
                    &view,
                    move |this, window, cx| {
                        this.file_name_dialog(
                            id,
                            NameAction::Directory(directory.clone()),
                            window,
                            cx,
                        )
                    },
                ))
                .item(menu_action("刷新", &view, move |this, _, cx| {
                    this.refresh_files(id, cx)
                }))
            })
            .into_any_element()
    }

    fn file_name_dialog(
        &self,
        id: u64,
        action: NameAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let (label, placeholder) = action.labels();
        let initial = action.initial();
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(initial)
        });
        let action = Rc::new(action);
        let view = cx.weak_entity();
        let error = Rc::new(RefCell::new(None::<&'static str>));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let action = action.clone();
            let current = state.clone();
            let error = error.clone();
            let message = *error.borrow();
            alert
                .confirm()
                .title(label)
                .ok_text(label)
                .cancel_text("取消")
                .child(
                    col()
                        .gap(px(5.))
                        .child(input(&state, placeholder, palette))
                        .children(message.map(|error| div().text_color(palette.warn).child(error))),
                )
                .on_ok(move |_, window, cx| {
                    let value = current.read(cx).value();
                    match action.operation(&value) {
                        Ok(operation) => {
                            let _ = view.update(cx, |this, cx| this.mutate_file(id, operation, cx));
                            true
                        }
                        Err(message) => {
                            *error.borrow_mut() = Some(message);
                            window.refresh();
                            false
                        }
                    }
                })
        });
    }

    fn confirm_delete(
        &self,
        id: u64,
        entry: RemoteEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let path = entry.path.clone();
            let directory = entry.is_dir;
            alert
                .confirm()
                .title("删除远端文件")
                .description(entry.path.clone())
                .ok_text("删除")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("取消")
                .on_ok(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.mutate_file(
                            id,
                            RemoteMutation::Delete {
                                path: path.clone(),
                                directory,
                            },
                            cx,
                        )
                    });
                    true
                })
        });
    }

    fn update_drop_target(
        &mut self,
        id: u64,
        point: Point<Pixels>,
        path: &str,
        count: usize,
        hovered: bool,
        folder: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id) else { return };
        let previous = &tab.files.drop_hover;
        if !folder
            && previous
                .as_ref()
                .is_some_and(|(position, _, _)| *position == point)
        {
            return;
        }
        let next = hovered.then(|| (point, path.to_owned(), count));
        let changed = previous.as_ref().map(|(_, path, count)| (path, count))
            != next.as_ref().map(|(_, path, count)| (path, count));
        tab.files.drop_hover = next;
        if changed {
            cx.notify();
        }
    }

    fn clear_drop_target(&mut self, id: u64) {
        if let Some(tab) = self.tab_mut(id) {
            tab.files.drop_hover = None;
        }
    }

    pub(in crate::workbench) fn file_input_drop(
        &self,
        id: &'static str,
        element: impl IntoElement,
        palette: Palette,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let active = self.active;
        let connected = self
            .active_tab()
            .is_some_and(|tab| tab.connection.is_some());
        col()
            .id(id)
            .w_full()
            .min_h_0()
            .child(element)
            .when(connected, |target| {
                target
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style
                            .bg(palette.tint)
                            .border_1()
                            .border_color(palette.accent)
                    })
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                        let Some(id) = active else {
                            return;
                        };
                        let Some(tab) = this.tab(id) else {
                            return;
                        };
                        let directory = tab.files.terminal_directory.clone().or_else(|| {
                            (!tab.files.home.is_empty()).then(|| tab.files.home.clone())
                        });
                        let Some(directory) = directory else {
                            this.announce("等待远端登录目录加载后再上传", cx);
                            return;
                        };
                        this.clear_drop_target(id);
                        this.upload(id, directory, paths.paths().to_vec(), true, cx);
                    }))
                    .on_drop(cx.listener(move |this, file: &FileDrag, window, cx| {
                        if Some(file.tab) == active {
                            this.append_file_path(file.tab, &file.entry.path, window, cx);
                        }
                    }))
            })
    }

    fn drag_entry(&self, id: u64, entry: RemoteEntry, cx: &mut Context<Self>) {
        let Some(connection) = self.tab(id).and_then(|tab| tab.connection.clone()) else {
            return;
        };
        let runtime = self.runtime.handle().clone();
        let events = self.events.clone();
        let options = TransferOptions {
            choice: TransferChoice::Ask,
            preserve_metadata: self.settings.transfer_preserve_metadata,
        };
        cx.spawn(async move |view, cx| {
            let notifications = runtime.clone();
            let name = entry.name.clone();
            let path = entry.path;
            let result = platform::drag_file(
                &entry.name,
                entry.size,
                &runtime,
                move |local| async move {
                    let (_signal, watch) = watch::channel(TransferSignal::Running);
                    let outcome = connection
                        .download_file(&path, &local, options, watch, |_, _| {})
                        .await?;
                    anyhow::ensure!(
                        matches!(outcome, TransferOutcome::Complete(_)),
                        "本地下载位置已存在同名文件"
                    );
                    Ok(())
                },
                move |result| {
                    let notice = match result {
                        Ok(()) => format!("已下载 {name}"),
                        Err(error) => format!("下载 {name} 失败 · {error:#}"),
                    };
                    notifications.spawn(async move {
                        let _ = events.send(Event::DragFinished(notice)).await;
                    });
                },
            );
            if let Err(error) = result {
                let _ = view.update(cx, |this, cx| {
                    this.announce(format!("无法拖出 {} · {error:#}", entry.name), cx)
                });
            }
        })
        .detach();
    }

    fn track_native_file_drag(&self, id: u64, cx: &Context<Self>) -> impl IntoElement {
        let movement = cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
            let viewport = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
            if event.pressed_button != Some(MouseButton::Left) || viewport.contains(&event.position)
            {
                return;
            }
            let Some(entry) = this
                .tab_mut(id)
                .and_then(|tab| tab.files.native_drag.take())
            else {
                return;
            };
            cx.stop_active_drag(window);
            this.drag_entry(id, entry, cx);
        });
        let release = cx.listener(move |this, _: &MouseUpEvent, _, _| {
            if let Some(tab) = this.tab_mut(id) {
                tab.files.native_drag = None;
            }
        });
        // Windows captures pointer movement outside the client area during the drag.
        // Hand off to OLE only at that boundary so drops inside the workbench stay in GPUI.
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        movement(event, window, cx);
                    }
                });
                window.on_mouse_event(move |event, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        release(event, window, cx);
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn render_transfers(
        &self,
        id: u64,
        files: &FilePane,
        palette: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<Div> {
        if files.transfers.is_empty() {
            return None;
        }
        let running = files
            .transfers
            .iter()
            .filter(|item| matches!(item.state, TransferState::Running))
            .count();
        let waiting = files
            .transfers
            .iter()
            .filter(|item| matches!(item.state, TransferState::Conflict(_)))
            .count();
        let paused = files
            .transfers
            .iter()
            .filter(|item| matches!(item.state, TransferState::Paused))
            .count();
        let queued = files
            .transfers
            .iter()
            .filter(|item| matches!(item.state, TransferState::Waiting))
            .count();
        let summary = if running > 0 {
            format!("{running} 个进行中")
        } else if queued > 0 {
            format!("{queued} 个排队中")
        } else if paused > 0 {
            format!("{paused} 个已暂停")
        } else if waiting == 0 {
            "队列已结束".into()
        } else {
            String::new()
        };
        let open = files.transfers_open;
        let resume = running == 0 && queued == 0 && paused > 0;
        let header = row()
            .h(px(32.))
            .pl(px(9.))
            .pr(px(5.))
            .gap(px(5.))
            .child(
                row()
                    .id("transfer-toggle")
                    .gap(px(5.))
                    .flex_1()
                    .cursor_pointer()
                    .tab_stop(true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(tab) = this.tab_mut(id) {
                            tab.files.transfers_open = !tab.files.transfers_open;
                        }
                        cx.notify();
                    }))
                    .child(small_icon(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        palette.muted,
                    ))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("传输"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(palette.muted)
                            .truncate()
                            .child(summary),
                    )
                    .when(waiting > 0, |header| {
                        header.child(
                            div()
                                .text_size(px(12.))
                                .text_color(palette.warn)
                                .child(format!("{waiting} 个待处理")),
                        )
                    }),
            )
            .child(
                file_tool(
                    "transfer-pause-all",
                    if resume {
                        IconName::Play
                    } else {
                        IconName::Pause
                    },
                    if resume {
                        "全部继续"
                    } else {
                        "全部暂停"
                    },
                    palette,
                )
                .disabled(running + queued + paused == 0)
                .on_click(
                    cx.listener(move |this, _, _, cx| this.control_transfer(id, None, false, cx)),
                ),
            );
        Some(
            col()
                .flex_shrink_0()
                .border_t_1()
                .border_color(palette.line)
                .child(header)
                .when(open, |transfers| {
                    transfers.child(
                        div()
                            .id("transfer-list")
                            .max_h(window.viewport_size().height * 0.32)
                            .overflow_y_scroll()
                            .children(files.transfers.iter().map(|item| {
                                self.render_transfer_item(id, item, files.conflict_all, palette, cx)
                            })),
                    )
                }),
        )
    }

    fn render_transfer_item(
        &self,
        tab: u64,
        item: &Transfer,
        apply_all: bool,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = item.id;
        let upload = item.source.is_upload();
        let pending = item.state.pending();
        let cancelling = matches!(item.state, TransferState::Cancelling);
        let conflict = matches!(item.state, TransferState::Conflict(_));
        let paused = matches!(item.state, TransferState::Paused);
        let icon = if matches!(item.state, TransferState::Complete(_)) {
            IconName::Check
        } else if upload {
            IconName::ArrowUpFromLine
        } else {
            IconName::ArrowDownToLine
        };
        let color = if matches!(item.state, TransferState::Complete(_)) {
            palette.green
        } else if upload {
            palette.accent
        } else {
            palette.muted
        };
        let header = row()
            .h(px(22.))
            .gap(px(6.))
            .child(small_icon(icon, color))
            .child(div().flex_1().truncate().child(item.name.clone()))
            .when(pending && !conflict && !cancelling, |header| {
                header.child(
                    file_tool(
                        ("transfer-pause", id),
                        if paused {
                            IconName::Play
                        } else {
                            IconName::Pause
                        },
                        if paused { "继续" } else { "暂停" },
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.control_transfer(tab, Some(id), false, cx)
                    })),
                )
            })
            .when(pending && !cancelling, |header| {
                header.child(
                    file_tool(("transfer-cancel", id), IconName::X, "取消传输", palette).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.control_transfer(tab, Some(id), true, cx)
                        }),
                    ),
                )
            });
        let details = match &item.state {
            TransferState::Conflict(conflict) => {
                let mut description = if conflict.directory {
                    "已存在同名文件夹".to_owned()
                } else {
                    "已存在同名文件".to_owned()
                };
                if let Some(size) = conflict.size.filter(|_| !conflict.directory) {
                    description.push_str(&format!("（{}", byte_size(size)));
                    if let Some(time) = file_time(conflict.mtime) {
                        description.push_str(&format!("，{} 修改", short_time(&time)));
                    }
                    description.push('）');
                }
                let choices = [
                    ("覆盖", TransferChoice::Overwrite),
                    ("跳过", TransferChoice::Skip),
                    ("保留两者", TransferChoice::KeepBoth),
                ];
                col()
                    .gap(px(5.))
                    .child(
                        row()
                            .gap(px(5.))
                            .text_color(palette.warn)
                            .child(small_icon(IconName::TriangleAlert, palette.warn))
                            .child(description),
                    )
                    .child(row().gap(px(5.)).children(choices.into_iter().map(
                        |(label, choice)| {
                            button((label, id), label, palette)
                                .h(px(23.))
                                .px(px(6.))
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .disabled(conflict.directory && choice == TransferChoice::Overwrite)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.resolve_transfer(tab, id, choice, cx)
                                }))
                        },
                    )))
                    .child(
                        row()
                            .id(("transfer-all", id))
                            .gap(px(5.))
                            .cursor_pointer()
                            .tab_stop(true)
                            .text_color(palette.muted)
                            .child(small_icon(
                                if apply_all {
                                    IconName::SquareCheck
                                } else {
                                    IconName::Square
                                },
                                palette.accent,
                            ))
                            .child("其余冲突同样处理")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(tab) = this.tab_mut(tab) {
                                    tab.files.conflict_all = !tab.files.conflict_all;
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            TransferState::Waiting | TransferState::Running | TransferState::Paused => {
                let percent = if item.total == 0 {
                    0
                } else {
                    (item.copied.saturating_mul(100) / item.total).min(100)
                };
                let details = if matches!(item.state, TransferState::Running) {
                    let eta = if item.speed > 0. {
                        format!(
                            "剩余 {} 秒",
                            (item.total.saturating_sub(item.copied) as f64 / item.speed).ceil()
                                as u64
                        )
                    } else {
                        "正在计算…".into()
                    };
                    format!("{percent}%   {}/s   {eta}", byte_size(item.speed as u64))
                } else if paused {
                    format!("{percent}%   已暂停")
                } else {
                    "排队中".into()
                };
                col()
                    .gap(px(5.))
                    .child(
                        div()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(palette.line)
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(percent as f32 / 100.))
                                    .bg(palette.accent),
                            ),
                    )
                    .child(div().text_color(palette.muted).truncate().child(details))
                    .child(div().text_color(palette.muted).truncate().child(format!(
                        "{} {}",
                        if upload { "到" } else { "存到" },
                        item.source.destination()
                    )))
                    .into_any_element()
            }
            TransferState::Complete(path) => div()
                .text_color(palette.muted)
                .truncate()
                .child(format!(
                    "{} {} · {}",
                    if upload { "已上传" } else { "已下载" },
                    byte_size(item.total),
                    path
                ))
                .into_any_element(),
            TransferState::Failed(error) => div()
                .text_color(palette.danger)
                .child(format!("传输失败 · {error}"))
                .into_any_element(),
            TransferState::Cancelled => div()
                .text_color(palette.muted)
                .child("已取消")
                .into_any_element(),
            TransferState::Cancelling => div()
                .text_color(palette.muted)
                .child("正在取消…")
                .into_any_element(),
            TransferState::Skipped => div()
                .text_color(palette.muted)
                .child("已跳过")
                .into_any_element(),
        };
        col()
            .px(px(8.))
            .pt(px(4.))
            .pb(px(7.))
            .gap(px(4.))
            .border_b_1()
            .border_color(palette.line)
            .text_size(px(13.))
            .when(conflict, |item| item.bg(palette.warn.opacity(0.12)))
            .child(header)
            .child(div().pl(px(20.)).text_size(px(12.)).child(details))
            .into_any_element()
    }
}

struct FileDragPreview(String, Palette);

impl Render for FileDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        row()
            .gap(px(6.))
            .px(px(8.))
            .py(px(4.))
            .bg(self.1.raised)
            .rounded(px(6.))
            .shadow_md()
            .text_size(px(12.))
            .child(small_icon(IconName::File, self.1.accent))
            .child(self.0.clone())
    }
}

impl FilePane {
    fn visible_row_count(&self, cx: &App) -> usize {
        let query = self
            .filter
            .as_ref()
            .filter(|_| self.filter_open)
            .map(|(state, _)| state.read(cx).value())
            .unwrap_or_default();
        let mut visible = self.visible_rows.borrow_mut();
        if self.branches.is_empty() && !self.hide_hidden && query.is_empty() {
            *visible = None;
            return self.entries.len();
        }
        if let Some(rows) = visible
            .as_ref()
            .filter(|rows| rows.query == query && rows.hide_hidden == self.hide_hidden)
        {
            return rows.entries.len();
        }

        let mut rows = VisibleFileRows {
            hide_hidden: self.hide_hidden,
            branches: Vec::new(),
            entries: Vec::new(),
            query,
        };
        let query = rows.query.to_lowercase();
        rows.append(self, &self.entries, &query, None, 0);
        let count = rows.entries.len();
        *visible = Some(rows);
        count
    }
}

struct FileRowIndex {
    branch: Option<usize>,
    index: usize,
    depth: usize,
}

pub(super) struct VisibleFileRows {
    query: SharedString,
    hide_hidden: bool,
    branches: Vec<String>,
    entries: Vec<FileRowIndex>,
}

impl VisibleFileRows {
    fn append(
        &mut self,
        files: &FilePane,
        entries: &[RemoteEntry],
        query: &str,
        branch: Option<usize>,
        depth: usize,
    ) {
        for (index, entry) in entries.iter().enumerate() {
            if self.hide_hidden && entry.hidden {
                continue;
            }
            if query.is_empty() || entry.is_dir || entry.name.to_lowercase().contains(query) {
                self.entries.push(FileRowIndex {
                    branch,
                    index,
                    depth,
                });
            }
            if let Some(Some(entries)) = files.branches.get(&entry.path) {
                let branch = self.branches.len();
                self.branches.push(entry.path.clone());
                self.append(files, entries, query, Some(branch), depth + 1);
            }
        }
    }

    fn entry<'a>(&self, files: &'a FilePane, index: usize) -> Option<(&'a RemoteEntry, usize)> {
        let row = self.entries.get(index)?;
        let entries = match row.branch {
            Some(branch) => files.branches.get(&self.branches[branch])?.as_ref()?,
            None => &files.entries,
        };
        Some((entries.get(row.index)?, row.depth))
    }
}

fn breadcrumbs(directory: &str, width: Pixels, local: bool) -> Vec<(String, String)> {
    let mut path = String::new();
    let mut parts: Vec<_> = if local {
        let mut parts = Path::new(directory)
            .ancestors()
            .filter(|path| path.is_absolute())
            .filter_map(|path| {
                let value = path.to_str()?;
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_else(|| value.trim_end_matches(['\\', '/']));
                Some((name.to_owned(), value.to_owned()))
            })
            .collect::<Vec<_>>();
        parts.reverse();
        parts
    } else {
        directory
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| {
                path.push('/');
                path.push_str(part);
                (part.to_owned(), path.clone())
            })
            .collect()
    };
    let max_chars = (f32::from(width) - 108.).max(16.) as usize / 7;
    let mut chars = parts
        .iter()
        .map(|(name, _)| name.chars().count() + 1)
        .sum::<usize>();
    let mut collapsed = None;
    while parts.len() > 1 && chars > max_chars {
        let (name, path) = parts.remove(0);
        chars = chars.saturating_sub(name.chars().count() + 1);
        collapsed = Some(path);
    }
    if let Some(path) = collapsed {
        parts.insert(0, ("…".into(), path));
    }
    parts
}

fn file_message(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .px(px(10.))
        .py(px(8.))
        .text_size(px(12.))
        .text_color(color)
        .child(text.into())
}
