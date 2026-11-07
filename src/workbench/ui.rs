use gpui_kit::component::button::{Button, ButtonVariants, DropdownButton};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{Disableable, Sizable, Theme, ThemeMode, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::{
    Drawer, Phase, Session, Tab, Workbench,
    files::{FileDrag, parent_path},
};
use crate::config::ConnectionProfile;
use crate::local::LocalShell;
use crate::platform;
use crate::ssh::RemoteEntry;

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) base: Hsla,
    pub(super) panel: Hsla,
    raised: Hsla,
    hover: Hsla,
    pub(super) line: Hsla,
    pub(super) ink: Hsla,
    pub(super) muted: Hsla,
    pub(super) dim: Hsla,
    pub(super) accent: Hsla,
    tint: Hsla,
    green: Hsla,
    pub(super) warn: Hsla,
    folder: Hsla,
    primary_text: Hsla,
}

impl Palette {
    pub(super) fn new(light: bool) -> Self {
        let colors = if light {
            [
                0xfaf9f7, 0xf2f1ef, 0xeae8e6, 0xe6e4e6, 0xd8d6da, 0x23232c, 0x50515e, 0x626575,
                0x4b508b, 0xe7e7f3, 0x4b508b, 0x815726, 0x565c85, 0xffffff,
            ]
        } else {
            [
                0x111216, 0x191a1f, 0x212229, 0x282a32, 0x30313a, 0xececf1, 0xa7a9b6, 0x898b9a,
                0xb6bcf2, 0x292b40, 0xb1b9e5, 0xd8b58d, 0xaeb5cc, 0x111216,
            ]
        };
        let [
            base,
            panel,
            raised,
            hover,
            line,
            ink,
            muted,
            dim,
            accent,
            tint,
            green,
            warn,
            folder,
            primary_text,
        ] = colors.map(|color| rgb(color).into());
        Self {
            base,
            panel,
            raised,
            hover,
            line,
            ink,
            muted,
            dim,
            accent,
            tint,
            green,
            warn,
            folder,
            primary_text,
        }
    }
}

pub(super) fn apply_theme(light: bool, cx: &mut App) {
    Theme::change(
        if light {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        },
        None,
        cx,
    );
    let palette = Palette::new(light);
    let has_cascadia = cx
        .text_system()
        .all_font_names()
        .iter()
        .any(|name| name == "Cascadia Mono");
    let primary_hover = rgb(if light { 0x44497e } else { 0xa6abdc }).into();
    Theme::update(cx, |theme| {
        theme.font_size = px(13.);
        if cfg!(target_os = "windows") {
            theme.font_family = "Segoe UI".into();
        }
        if has_cascadia {
            theme.mono_font_family = "Cascadia Mono".into();
        }
        theme.radius = px(6.);
        theme.radius_lg = px(11.);
        theme.colors.background = palette.base;
        theme.colors.foreground = palette.ink;
        theme.colors.border = palette.line;
        theme.colors.input = palette.line;
        theme.colors.ring = palette.accent;
        theme.colors.accent = palette.tint;
        theme.colors.accent_foreground = palette.accent;
        theme.colors.muted = palette.raised;
        theme.colors.muted_foreground = palette.dim;
        theme.colors.primary = palette.accent;
        theme.colors.primary_foreground = palette.primary_text;
        theme.colors.button_primary = palette.accent;
        theme.colors.button_primary_foreground = palette.primary_text;
        theme.colors.button_primary_hover = primary_hover;
        theme.colors.button_primary_active = primary_hover;
        theme.colors.secondary = palette.panel;
        theme.colors.secondary_foreground = palette.ink;
        theme.colors.secondary_hover = palette.hover;
        theme.colors.popover = palette.panel;
        theme.colors.popover_foreground = palette.ink;
    });
}

pub(super) fn row() -> Div {
    div().flex().flex_row().items_center().min_w_0()
}
pub(super) fn col() -> Div {
    div().flex().flex_col().min_w_0().min_h_0()
}

pub(super) fn input(state: &Entity<InputState>, label: &'static str, palette: Palette) -> Input {
    Input::new(state)
        .aria_label(label)
        .h(px(33.))
        .w_full()
        .min_w_0()
        .bg(palette.base)
        .border_color(palette.line)
        .rounded(px(6.))
}

pub(super) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    palette: Palette,
) -> Button {
    Button::new(id)
        .ghost()
        .label(label)
        .h(px(30.))
        .px(px(9.))
        .rounded(px(6.))
        .border_1()
        .border_color(palette.line)
        .text_size(px(13.))
}

pub(super) fn primary(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    palette: Palette,
) -> Button {
    button(id, label, palette)
        .primary()
        .border_color(palette.accent)
        .font_weight(FontWeight::SEMIBOLD)
}

pub(super) fn tool(
    id: impl Into<ElementId>,
    glyph: impl Into<SharedString>,
    title: &'static str,
    palette: Palette,
) -> Button {
    Button::new(id)
        .ghost()
        .label(glyph)
        .accessibility_label(title)
        .tooltip(title)
        .h(px(30.))
        .min_w(px(30.))
        .px(px(6.))
        .rounded(px(6.))
        .text_size(px(13.))
        .text_color(palette.muted)
}

pub(super) fn pane_head(palette: Palette) -> Div {
    row()
        .h(px(48.))
        .flex_shrink_0()
        .justify_between()
        .gap(px(6.))
        .pl(px(15.))
        .pr(px(12.))
        .border_b_1()
        .border_color(palette.line)
        .font_weight(FontWeight::SEMIBOLD)
}

pub(super) fn badge(label: &'static str, palette: Palette, mono: SharedString) -> Div {
    div()
        .border_1()
        .border_color(palette.line)
        .rounded(px(4.))
        .px(px(5.))
        .py(px(2.))
        .font_family(mono)
        .text_size(px(10.))
        .text_color(palette.muted)
        .font_weight(FontWeight::NORMAL)
        .child(label)
}

fn eyebrow(label: &'static str, palette: Palette) -> Div {
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::BOLD)
        .text_color(palette.dim)
        .child(label)
}

fn status_dot(color: Hsla) -> Div {
    div().size(px(7.)).rounded_full().bg(color).flex_shrink_0()
}

fn muted_message(message: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .px(px(12.))
        .py(px(11.))
        .text_size(px(12.))
        .text_color(palette.muted)
        .child(message.into())
}

fn menu_action(
    label: &'static str,
    view: &WeakEntity<Workbench>,
    action: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
) -> PopupMenuItem {
    let view = view.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        let _ = view.update(cx, |this, cx| action(this, window, cx));
    })
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width;
        let compact = width < px(760.);
        let right_inline = width >= px(1100.);
        if compact != self.compact {
            self.compact = compact;
            for tab in &self.tabs {
                tab.terminal
                    .update(cx, |terminal, cx| terminal.set_compact(compact, cx));
            }
        }
        let palette = Palette::new(self.settings.light);
        let mono = Theme::global(cx).mono_font_family.clone();
        let font = Theme::global(cx).font_family.clone();
        let left_width = if right_inline { px(276.) } else { px(240.) };
        let workspace = div()
            .flex()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .when(!compact, |workspace| {
                workspace.child(self.files(left_width, palette, mono.clone(), cx))
            })
            .child(self.middle(palette, mono.clone(), cx))
            .when(right_inline, |workspace| {
                workspace.child(self.sessions(palette, mono.clone(), cx))
            })
            .when(compact && self.drawer == Some(Drawer::Files), |workspace| {
                workspace.child(
                    self.files(px(276.).min(width * 0.85), palette, mono.clone(), cx)
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .shadow_lg()
                        .occlude(),
                )
            })
            .when(
                !right_inline && self.drawer == Some(Drawer::Sessions),
                |workspace| {
                    workspace.child(
                        self.sessions(palette, mono.clone(), cx)
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .shadow_lg()
                            .occlude(),
                    )
                },
            );
        col()
            .relative()
            .size_full()
            .font_family(font)
            .text_size(px(13.))
            .text_color(palette.ink)
            .bg(palette.base)
            .child(self.top(compact, right_inline, palette, mono.clone(), cx))
            .child(workspace)
            .child(self.status(compact, palette, mono, cx))
            .when(platform::drag_supported(), |root| {
                root.child(self.file_drag_tracking(cx))
            })
    }
}

impl Workbench {
    fn top(
        &self,
        compact: bool,
        right_inline: bool,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> TitleBar {
        TitleBar::new()
            .h(px(52.))
            .bg(palette.panel)
            .border_color(palette.line)
            .pl(px(if compact { 8. } else { 16. }))
            .child(
                row()
                    .flex_shrink_0()
                    .gap(px(9.))
                    .child(
                        row()
                            .size(px(29.))
                            .justify_center()
                            .rounded(px(7.))
                            .bg(palette.tint)
                            .font_family(mono)
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(palette.accent)
                            .child("›_"),
                    )
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("CapTerm"),
                    )
                    .when(!compact, |brand| {
                        brand
                            .child(div().mx(px(5.)).w(px(1.)).h(px(20.)).bg(palette.line))
                            .child(div().text_color(palette.dim).child("SSH Workbench"))
                    }),
            )
            .child(
                row()
                    .gap(px(8.))
                    .pr(px(if compact { 8. } else { 16. }))
                    .when(compact, |tools| {
                        tools.child(button("files-toggle", "Files", palette).on_click(
                            cx.listener(|this, _, _, cx| this.toggle_drawer(Drawer::Files, cx)),
                        ))
                    })
                    .when(!right_inline, |tools| {
                        tools.child(button("sessions-toggle", "Sessions", palette).on_click(
                            cx.listener(|this, _, _, cx| this.toggle_drawer(Drawer::Sessions, cx)),
                        ))
                    })
                    .child(
                        tool(
                            "theme",
                            if self.settings.light { "☾" } else { "☼" },
                            "Toggle light and dark theme",
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                    )
                    .when(cfg!(windows) && !compact, |tools| {
                        tools.child(self.local_shell_button(false, cx))
                    })
                    .child(primary("new-session", "＋ New session", palette).on_click(
                        cx.listener(|this, _, window, cx| this.profile_dialog(None, window, cx)),
                    )),
            )
    }

    fn local_shell_button(&self, compact: bool, cx: &Context<Self>) -> DropdownButton {
        let view = cx.weak_entity();
        DropdownButton::new("local-shell")
            .secondary()
            .outline()
            .with_size(px(30.))
            .flex_shrink_0()
            .menu_accessibility_label("Choose a local shell")
            .button(
                Button::new("open-powershell")
                    .label(if compact { "Local" } else { "＋ Local shell" })
                    .h(px(30.))
                    .px(px(9.))
                    .text_size(px(13.))
                    .tooltip("Open a local PowerShell tab")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.launch_local(LocalShell::PowerShell, window, cx);
                    })),
            )
            .dropdown_menu(move |menu, _, _| {
                [
                    LocalShell::PowerShell,
                    LocalShell::CommandPrompt,
                    LocalShell::Wsl,
                ]
                .into_iter()
                .fold(menu, |menu, shell| {
                    menu.item(menu_action(
                        shell.label(),
                        &view,
                        move |this, window, cx| {
                            this.launch_local(shell, window, cx);
                        },
                    ))
                })
            })
    }

    fn toggle_drawer(&mut self, drawer: Drawer, cx: &mut Context<Self>) {
        self.drawer = (self.drawer != Some(drawer)).then_some(drawer);
        cx.notify();
    }

    fn files(
        &self,
        width: Pixels,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let tab = self.active_tab();
        let active = self.active;
        let local = tab.is_some_and(Tab::is_local);
        let connected = tab.is_some_and(|tab| tab.connection.is_some());
        let directory = tab
            .map(|tab| tab.files.directory.as_str())
            .filter(|path| !path.is_empty());
        let can_upload = connected && directory.is_some();
        let directory = directory.unwrap_or("—");
        let refresh = tool("refresh-files", "↻", "Refresh files", palette)
            .disabled(!connected)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(id) = active {
                    let dir = this.upload_directory(id).unwrap_or_else(|| ".".into());
                    this.list(id, dir, cx);
                }
            }));
        let upload = tool("upload-files", "↑", "Upload files", palette)
            .disabled(!can_upload)
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(id) = active {
                    this.pick_upload(id, window, cx);
                }
            }));
        let path = row()
            .mx(px(11.))
            .mt(px(12.))
            .mb(px(9.))
            .h(px(33.))
            .flex_shrink_0()
            .gap(px(5.))
            .border_1()
            .border_color(palette.line)
            .rounded(px(6.))
            .bg(palette.base)
            .pr(px(7.))
            .font_family(mono.clone())
            .text_size(px(11.))
            .overflow_hidden()
            .child(
                tool("parent-folder", "↑", "Parent folder", palette)
                    .h_full()
                    .w(px(29.))
                    .border_r_1()
                    .border_color(palette.line)
                    .rounded(px(0.))
                    .text_color(palette.accent)
                    .disabled(!can_upload || directory == "/")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(id) = active
                            && let Some(dir) = this.upload_directory(id)
                        {
                            this.list(id, parent_path(&dir), cx);
                        }
                    })),
            )
            .child(div().min_w_0().truncate().child(directory.to_owned()));
        let scroll = div()
            .id("remote-file-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let scroll = match tab {
            Some(tab) if tab.is_local() => scroll.child(muted_message(
                "SFTP is available for SSH sessions.",
                palette,
            )),
            Some(tab) if tab.phase == Phase::Connected => {
                let scroll = if let Some(error) = &tab.files.error {
                    scroll.child(
                        muted_message(format!("SFTP unavailable · {error}"), palette)
                            .text_color(palette.warn),
                    )
                } else if tab.files.loading && tab.files.entries.is_empty() {
                    scroll.child(muted_message("Loading remote directory…", palette))
                } else if tab.files.entries.is_empty() {
                    scroll.child(muted_message("This directory is empty.", palette))
                } else {
                    scroll
                };
                scroll.children(
                    tab.files
                        .entries
                        .iter()
                        .map(|entry| self.file_row(tab, entry, palette, mono.clone(), cx)),
                )
            }
            Some(tab) if tab.phase == Phase::Connecting => {
                scroll.child(muted_message("Waiting for SSH to connect…", palette))
            }
            _ => scroll.child(muted_message(
                "Connect to a host to browse remote files.",
                palette,
            )),
        };
        let dropzone = col()
            .id("upload-dropzone")
            .mx(px(11.))
            .my(px(11.))
            .px(px(9.))
            .py(px(12.))
            .flex_shrink_0()
            .gap(px(3.))
            .rounded(px(7.))
            .border_1()
            .border_dashed()
            .border_color(palette.line)
            .text_center()
            .text_size(px(11.))
            .text_color(palette.muted)
            .child(
                div()
                    .text_color(palette.accent)
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("↑ Drop files to upload"),
            )
            .child(if platform::drag_supported() {
                "Drag remote files out to download"
            } else {
                "Use the file menu to download"
            })
            .when(can_upload, |dropzone| {
                dropzone
                    .cursor_pointer()
                    .tab_stop(true)
                    .hover(|style| style.bg(palette.tint).border_color(palette.accent))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.bg(palette.tint).border_color(palette.accent)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(id) = active {
                            this.pick_upload(id, window, cx);
                        }
                    }))
                    .on_drop::<ExternalPaths>(cx.listener(
                        move |this, paths: &ExternalPaths, _, cx| {
                            if let Some(id) = active
                                && let Some(directory) = this.upload_directory(id)
                            {
                                this.upload(id, directory, paths.paths().to_vec(), cx);
                            }
                        },
                    ))
            });
        col()
            .w(width)
            .flex_shrink_0()
            .bg(palette.panel)
            .border_r_1()
            .border_color(palette.line)
            .child(
                pane_head(palette)
                    .child(row().gap(px(8.)).child("Remote files").child(badge(
                        "SFTP",
                        palette,
                        mono.clone(),
                    )))
                    .child(row().gap(px(2.)).child(refresh).child(upload)),
            )
            .when(!local, |pane| {
                pane.child(path).child(
                    row()
                        .pb(px(7.))
                        .px(px(11.))
                        .justify_between()
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(palette.line)
                        .child(eyebrow("NAME", palette))
                        .child(eyebrow("SIZE", palette)),
                )
            })
            .child(scroll)
            .when(!local, |pane| pane.child(dropzone))
    }

    fn file_drag_tracking(&self, cx: &Context<Self>) -> impl IntoElement {
        let reset = cx.listener(|this, event: &MouseDownEvent, _, _| {
            if event.button == MouseButton::Left {
                this.file_drag = None;
            }
        });
        let track = cx.listener(|this, event: &MouseMoveEvent, _, cx| {
            let Some(drag) = &mut this.file_drag else {
                return;
            };
            if drag.started {
                return;
            }
            if event.pressed_button != Some(MouseButton::Left) {
                this.file_drag = None;
            } else if (event.position - drag.origin).magnitude() > 5. {
                drag.started = true;
                let (id, entry) = (drag.tab, drag.entry.clone());
                this.drag_entry(id, entry, cx);
            }
        });
        // The mouse may already be outside the file row when it passes the
        // drag threshold. Track the gesture at window scope until OLE takes it.
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        reset(event, window, cx);
                    }
                });
                window.on_mouse_event(move |event, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        track(event, window, cx);
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn file_row(
        &self,
        tab: &Tab,
        entry: &RemoteEntry,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = tab.id;
        let selected = tab.files.selected.as_ref() == Some(&entry.path);
        let drag = !entry.is_dir && platform::drag_supported();
        let file = entry.clone();
        let on_open = cx.listener(move |this, event: &ClickEvent, _, cx| {
            if matches!(event, ClickEvent::Mouse(_))
                && this.file_drag.take().is_some_and(|drag| {
                    drag.started && drag.tab == id && drag.entry.path == file.path
                })
            {
                return;
            }
            this.open_entry(id, file.clone(), cx);
        });
        let row = row()
            .id(format!("remote-file-{}", entry.path))
            .h(px(32.))
            .gap(px(8.))
            .px(px(10.))
            .cursor_pointer()
            .tab_stop(true)
            .hover(|style| style.bg(palette.hover))
            .focus_visible(|style| style.bg(palette.hover))
            .when(selected, |row| row.bg(palette.tint))
            .on_click(on_open)
            .child(
                div()
                    .w(px(15.))
                    .flex_shrink_0()
                    .text_center()
                    .text_color(palette.folder)
                    .child(if entry.is_dir { "▱" } else { "▤" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(entry.name.clone()),
            )
            .child(
                div()
                    .w(px(55.))
                    .flex_shrink_0()
                    .text_right()
                    .font_family(mono)
                    .text_size(px(11.))
                    .text_color(palette.muted)
                    .child(
                        entry
                            .size
                            .filter(|_| !entry.is_dir)
                            .map_or_else(|| "—".into(), file_size),
                    ),
            );
        if entry.is_dir {
            return row.into_any_element();
        }
        let row = row.when(drag, |row| {
            let file = entry.clone();
            row.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.file_drag = Some(FileDrag {
                        tab: id,
                        entry: file.clone(),
                        origin: event.position,
                        started: false,
                    });
                }),
            )
        });
        let file = entry.clone();
        let view = cx.weak_entity();
        row.context_menu(move |menu, _, _| {
            let editor = file.clone();
            let download = file.clone();
            menu.item(menu_action(
                "Edit remote text",
                &view,
                move |this, _, cx| this.open_entry(id, editor.clone(), cx),
            ))
            .item(menu_action(
                "Download…",
                &view,
                move |this, window, cx| this.pick_download(id, download.clone(), window, cx),
            ))
        })
        .into_any_element()
    }

    fn middle(&self, palette: Palette, mono: SharedString, cx: &Context<Self>) -> Div {
        let tab = self.active_tab();
        let active = self.active;
        let raw = tab.is_some_and(|tab| tab.terminal.read(cx).raw());
        let connected = tab.is_some_and(|tab| tab.phase == Phase::Connected);
        let tabs = div()
            .id("terminal-tabs")
            .flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .items_stretch()
            .overflow_x_scroll()
            .children(
                self.tabs
                    .iter()
                    .map(|tab| self.session_tab(tab, palette, cx)),
            )
            .child(
                row().child(tool("add-tab", "＋", "New session tab", palette).on_click(
                    cx.listener(|this, _, window, cx| this.profile_dialog(None, window, cx)),
                )),
            );
        let ssh_profile = tab.and_then(Tab::ssh_profile);
        let route = tab
            .map(|tab| match &tab.session {
                Session::Ssh(profile) => target(profile),
                Session::Local(shell) => format!("Local · {}", shell.label()),
            })
            .unwrap_or_else(|| "SSH terminal".into());
        let route = row()
            .gap(px(8.))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .font_family(mono.clone())
            .text_size(px(12.))
            .child(div().min_w_0().truncate().child(route))
            .when_some(
                ssh_profile.filter(|profile| !profile.jumps.is_empty()),
                |route, profile| {
                    route.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(palette.muted)
                            .child(format!("via {}", route_hops(profile))),
                    )
                },
            );
        let controls = row()
            .flex_shrink_0()
            .gap(px(8.))
            .child(
                tool(
                    "terminal-meta",
                    "# Time",
                    "Toggle line numbers and timestamps",
                    palette,
                )
                .disabled(tab.is_none() || raw)
                .when(tab.is_some_and(|tab| tab.gutter) && !raw, |button| {
                    button.bg(palette.tint).text_color(palette.accent)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(tab) = active.and_then(|id| this.tab_mut(id)) {
                        tab.gutter = !tab.gutter;
                        tab.terminal
                            .update(cx, |terminal, cx| terminal.set_gutter(tab.gutter, cx));
                        cx.notify();
                    }
                })),
            )
            .child(
                tool(
                    "terminal-mode",
                    "TUI",
                    "Toggle direct terminal keyboard input",
                    palette,
                )
                .disabled(tab.is_none())
                .when(raw, |button| {
                    button.bg(palette.tint).text_color(palette.accent)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(tab) = active.and_then(|id| this.tab_mut(id)) {
                        tab.manual_raw = !tab.manual_raw;
                        tab.terminal.update(cx, |terminal, cx| {
                            terminal.set_raw(tab.manual_raw, cx);
                            terminal.focus(window, cx);
                        });
                        cx.notify();
                    }
                })),
            );
        let surface = div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(palette.base)
            .when_some(tab, |surface, tab| surface.child(tab.terminal.clone()))
            .when(
                tab.is_none_or(|tab| tab.phase != Phase::Connected),
                |surface| surface.child(self.empty_terminal(tab, palette, mono.clone(), cx)),
            );
        let draft = if raw {
            &self.raw_draft
        } else {
            tab.map_or(&self.empty_draft, |tab| &tab.draft)
        };
        let composer = row()
            .h(px(if self.compact { 55. } else { 61. }))
            .flex_shrink_0()
            .gap(px(8.))
            .px(px(12.))
            .border_t_1()
            .border_color(palette.line)
            .bg(palette.panel)
            .child(
                div()
                    .font_family(mono.clone())
                    .text_color(palette.accent)
                    .child("›_"),
            )
            .child(
                div().flex_1().min_w_0().child(
                    input(draft, "Send a command", palette)
                        .font_family(mono)
                        .text_size(px(12.))
                        .disabled(!connected || raw),
                ),
            )
            .child(
                primary("send-command", "Send ↵", palette)
                    .disabled(!connected || raw)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(id) = active {
                            this.send_draft(id, window, cx);
                        }
                    })),
            );
        col()
            .flex_1()
            .overflow_hidden()
            .child(
                row()
                    .h(px(41.))
                    .flex_shrink_0()
                    .px(px(7.))
                    .gap(px(7.))
                    .bg(palette.panel)
                    .border_b_1()
                    .border_color(palette.line)
                    .child(tabs)
                    .when(cfg!(windows) && self.compact, |row| {
                        row.child(self.local_shell_button(true, cx))
                    }),
            )
            .child(
                row()
                    .h(px(43.))
                    .flex_shrink_0()
                    .justify_between()
                    .gap(px(6.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(palette.line)
                    .child(route)
                    .child(controls),
            )
            .child(surface)
            .child(composer)
    }

    fn session_tab(&self, tab: &Tab, palette: Palette, cx: &Context<Self>) -> Div {
        let id = tab.id;
        let active = self.active == Some(id);
        row()
            .flex_shrink_0()
            .gap(px(5.))
            .pl(px(12.))
            .pr(px(6.))
            .border_b_2()
            .border_color(if active {
                palette.accent
            } else {
                gpui_kit::transparent_black()
            })
            .when(active, |row| row.bg(palette.base))
            .child(
                row()
                    .id(("terminal-tab", id))
                    .h_full()
                    .tab_stop(true)
                    .cursor_pointer()
                    .gap(px(8.))
                    .text_color(if active { palette.ink } else { palette.muted })
                    .focus_visible(|style| style.text_color(palette.accent))
                    .child(status_dot(phase_color(tab.phase, palette)))
                    .child(
                        div()
                            .max_w(px(190.))
                            .truncate()
                            .child(tab.name().to_owned()),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(id, window, cx)),
                    ),
            )
            .child(
                tool(("close-tab", id), "×", "Close terminal tab", palette)
                    .w(px(24.))
                    .min_w(px(24.))
                    .h(px(24.))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.close_tab(id, window, cx)),
                    ),
            )
    }

    fn empty_terminal(
        &self,
        tab: Option<&Tab>,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let Some(tab) = tab else {
            return col()
                .absolute()
                .top(px(20.))
                .left(px(18.))
                .right(px(18.))
                .gap(px(9.))
                .max_w(px(460.))
                .child(
                    div()
                        .font_family(mono)
                        .text_color(palette.accent)
                        .font_weight(FontWeight::BOLD)
                        .child("›_  CapTerm SSH Workbench"),
                )
                .child(div().text_color(palette.muted).child(
                    if self.settings.profiles.is_empty() {
                        "Create a session or import an OpenSSH config to connect to a host."
                    } else {
                        "Select a saved session to open an SSH terminal."
                    },
                ))
                .child(
                    row()
                        .gap(px(8.))
                        .mt(px(5.))
                        .child(button("welcome-new", "＋ New session", palette).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.profile_dialog(None, window, cx)
                            }),
                        ))
                        .child(
                            button("welcome-import", "↓ Import SSH config", palette).on_click(
                                cx.listener(|this, _, window, cx| this.import_dialog(window, cx)),
                            ),
                        ),
                );
        };
        let message = match &tab.session {
            Session::Ssh(profile) => match tab.phase {
                Phase::Connecting => format!("Connecting to {}…", target(profile)),
                Phase::Failed => format!("Could not connect to {}", target(profile)),
                _ => format!("Disconnected from {}", target(profile)),
            },
            Session::Local(shell) => match tab.phase {
                Phase::Connecting => format!("Starting {}…", shell.label()),
                Phase::Failed => format!("{} failed", shell.label()),
                _ => format!("{} exited", shell.label()),
            },
        };
        col()
            .absolute()
            .top(px(13.))
            .left(px(14.))
            .right(px(14.))
            .gap(px(7.))
            .bg(palette.base)
            .p(px(5.))
            .child(
                div()
                    .font_family(mono)
                    .text_color(phase_color(tab.phase, palette))
                    .child(message),
            )
            .when_some(
                tab.ssh_profile().filter(|_| tab.phase != Phase::Connecting),
                |view, profile| {
                    let profile = profile.clone();
                    let password_profile = profile.clone();
                    view.child(
                        row()
                            .gap(px(8.))
                            .child(
                                button("reconnect", "Reconnect", palette).on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.connect(profile.clone(), None, window, cx);
                                    },
                                )),
                            )
                            .child(
                                button("reconnect-password", "Connect with password", palette)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.password_dialog(password_profile.clone(), window, cx);
                                    })),
                            ),
                    )
                },
            )
    }

    fn sessions(&self, palette: Palette, mono: SharedString, cx: &Context<Self>) -> Div {
        let filter = self.filter.read(cx).value().trim().to_lowercase();
        let mut matches = 0;
        let mut list = div().id("saved-session-list").min_h_0().overflow_y_scroll();
        for (imported, group) in [(false, "SAVED"), (true, "IMPORTED")] {
            let profiles = self
                .settings
                .profiles
                .iter()
                .filter(|profile| {
                    profile.source.is_some() == imported
                        && (filter.is_empty()
                            || format!(
                                "{} {} {} {}",
                                profile.name,
                                profile.host,
                                profile.user,
                                route_hops(profile)
                            )
                            .to_lowercase()
                            .contains(&filter))
                })
                .collect::<Vec<_>>();
            if !profiles.is_empty() {
                matches += profiles.len();
                list = list
                    .child(eyebrow(group, palette).mx(px(6.)).my(px(5.)))
                    .children(
                        profiles
                            .into_iter()
                            .map(|profile| self.profile_row(profile, palette, mono.clone(), cx)),
                    );
            }
        }
        if matches == 0 {
            list = list.child(muted_message(
                if self.settings.profiles.is_empty() {
                    "No saved sessions yet."
                } else {
                    "No sessions match this filter."
                },
                palette,
            ));
        }
        col()
            .w(px(244.))
            .flex_shrink_0()
            .bg(palette.panel)
            .border_l_1()
            .border_color(palette.line)
            .child(pane_head(palette).child("Saved sessions").child(
                tool("add-saved-session", "＋", "Add saved session", palette).on_click(
                    cx.listener(|this, _, window, cx| this.profile_dialog(None, window, cx)),
                ),
            ))
            .child(
                col()
                    .flex_1()
                    .px(px(9.))
                    .py(px(11.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .mb(px(16.))
                            .child(input(&self.filter, "Filter sessions", palette).h(px(32.))),
                    )
                    .child(list.flex_1()),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(palette.line)
                    .px(px(9.))
                    .py(px(10.))
                    .flex_shrink_0()
                    .child(
                        button("import-config", "↓ Import SSH config", palette)
                            .w_full()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.import_dialog(window, cx)),
                            ),
                    ),
            )
            .child(self.monitor(palette, mono))
    }

    fn profile_row(
        &self,
        profile: &ConnectionProfile,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let tab = self.tabs.iter().find(|tab| {
            tab.ssh_profile()
                .is_some_and(|connected| connected.id == profile.id)
        });
        let selected = tab.is_some_and(|tab| Some(tab.id) == self.active);
        let testing = self
            .tests
            .values()
            .any(|(profile_id, _)| *profile_id == profile.id);
        let color = if testing {
            palette.warn
        } else {
            tab.map_or(palette.dim, |tab| phase_color(tab.phase, palette))
        };
        let subtitle = if profile.jumps.is_empty() {
            format!("{} · {}", profile.user, profile.host)
        } else {
            format!("{} · via {}", profile.user, route_hops(profile))
        };
        let connect = profile.clone();
        let test = profile.clone();
        let context_profile = profile.clone();
        let view = cx.weak_entity();
        row()
            .id(format!("saved-session-{}", profile.id))
            .min_h(px(55.))
            .gap(px(8.))
            .py(px(7.))
            .pl(px(8.))
            .pr(px(5.))
            .border_1()
            .border_color(if selected {
                palette.line
            } else {
                gpui_kit::transparent_black()
            })
            .rounded(px(6.))
            .hover(|style| style.bg(palette.hover))
            .when(selected, |row| row.bg(palette.tint))
            .child(status_dot(color))
            .child(
                col()
                    .id("connect-saved-session")
                    .flex_1()
                    .cursor_pointer()
                    .tab_stop(true)
                    .focus_visible(|style| style.text_color(palette.accent))
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(profile.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(mono)
                            .text_size(px(10.))
                            .text_color(palette.muted)
                            .child(subtitle),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.connect(connect.clone(), None, window, cx)
                    })),
            )
            .child(
                tool("test-session", "⌁", "Test SSH connection", palette)
                    .w(px(26.))
                    .min_w(px(26.))
                    .h(px(26.))
                    .text_size(px(16.))
                    .text_color(palette.accent)
                    .disabled(testing)
                    .on_click(cx.listener(move |this, _, _, cx| this.test(test.clone(), None, cx))),
            )
            .context_menu(move |menu, _, _| {
                let connect = context_profile.clone();
                let password = context_profile.clone();
                let test = context_profile.clone();
                let edit = context_profile.clone();
                let remove = context_profile.clone();
                menu.item(menu_action("Connect", &view, move |this, window, cx| {
                    this.connect(connect.clone(), None, window, cx)
                }))
                .item(menu_action(
                    "Connect with password…",
                    &view,
                    move |this, window, cx| this.password_dialog(password.clone(), window, cx),
                ))
                .item(menu_action(
                    "Test with password…",
                    &view,
                    move |this, window, cx| this.test_with_password(test.clone(), window, cx),
                ))
                .separator()
                .item(menu_action(
                    "Edit session…",
                    &view,
                    move |this, window, cx| this.profile_dialog(Some(edit.clone()), window, cx),
                ))
                .item(menu_action(
                    "Delete session…",
                    &view,
                    move |this, window, cx| this.remove_profile(remove.clone(), window, cx),
                ))
            })
            .into_any_element()
    }

    fn monitor(&self, palette: Palette, mono: SharedString) -> Div {
        let tab = self.active_tab();
        let panel = col()
            .h(px(116.))
            .flex_shrink_0()
            .gap(px(6.))
            .px(px(9.))
            .py(px(8.))
            .border_t_1()
            .border_color(palette.line);
        if tab.is_some_and(Tab::is_local) {
            return panel.child(eyebrow("HOST MONITOR", palette)).child(
                div()
                    .text_size(px(12.))
                    .text_color(palette.muted)
                    .child("Host monitoring is available for SSH sessions."),
            );
        }
        let sample = tab.and_then(|tab| tab.sample.as_ref());
        let data = sample.map(|(data, _)| data);
        let cpu = data.and_then(|data| data.cpu);
        let ram = data.and_then(|data| data.memory);
        let disk = data.and_then(|data| data.disk);
        let rtt = data.and_then(|data| data.rtt_ms);
        let sample_label = sample
            .map(|(metrics, sampled)| {
                let platform = if metrics.memory.is_some() || metrics.uptime_secs.is_some() {
                    "Linux"
                } else {
                    metrics
                        .os
                        .as_deref()
                        .and_then(|os| os.split_whitespace().next())
                        .unwrap_or("SSH")
                };
                format!("{platform} · {}s ago", sampled.elapsed().as_secs())
            })
            .unwrap_or_else(|| {
                if tab.is_some_and(|tab| tab.phase == Phase::Connected) {
                    "Waiting for sample"
                } else {
                    "No active host"
                }
                .into()
            });
        let memory = data
            .and_then(|data| Some((data.memory_used_bytes?, data.memory_total_bytes?)))
            .map(|(used, total)| memory_size(used, total))
            .unwrap_or_else(|| percent(ram));
        panel
            .child(
                row()
                    .h(px(15.))
                    .flex_shrink_0()
                    .justify_between()
                    .gap(px(6.))
                    .child(eyebrow("HOST MONITOR", palette).flex_shrink_0())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(palette.dim)
                            .child(sample_label),
                    ),
            )
            .child(
                row()
                    .gap(px(6.))
                    .child(metric("CPU", percent(cpu), cpu, palette, mono.clone()))
                    .child(metric("RAM", memory, ram, palette, mono.clone())),
            )
            .child(
                row()
                    .gap(px(6.))
                    .child(metric("Disk", percent(disk), disk, palette, mono.clone()))
                    .child(metric(
                        "RTT",
                        rtt.map_or_else(|| "—".into(), |value| format!("{value} ms")),
                        rtt.map(|value| value as f64 / 2.),
                        palette,
                        mono,
                    )),
            )
    }

    fn status(
        &self,
        compact: bool,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let tab = self.active_tab();
        let phase = tab.map_or(Phase::Disconnected, |tab| tab.phase);
        let local = tab.is_some_and(Tab::is_local);
        let color = phase_color(phase, palette);
        row()
            .h(px(29.))
            .flex_shrink_0()
            .justify_between()
            .gap(px(8.))
            .px(px(13.))
            .border_t_1()
            .border_color(palette.line)
            .bg(palette.panel)
            .font_family(mono)
            .text_size(px(11.))
            .text_color(palette.muted)
            .overflow_hidden()
            .child(
                row()
                    .flex_shrink_0()
                    .gap(px(8.))
                    .child(status_dot(color))
                    .child(
                        div()
                            .text_color(color)
                            .font_weight(FontWeight::MEDIUM)
                            .child(phase.label(local)),
                    )
                    .when(local, |row| row.child("· Local"))
                    .when(!compact && !local && phase == Phase::Connected, |row| {
                        row.child("· SSH 2 · xterm-256color · UTF-8")
                    }),
            )
            .child(
                div()
                    .id("status-notice")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_center()
                    .child(self.notice.clone())
                    .when(!self.notice.is_empty(), |notice| {
                        let detail = self.notice.clone();
                        notice.cursor_pointer().tab_stop(true).on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.notice_dialog(detail.clone(), window, cx)
                            },
                        ))
                    }),
            )
            .child(div().flex_shrink_0().child("CapTerm"))
    }
}

fn phase_color(phase: Phase, palette: Palette) -> Hsla {
    match phase {
        Phase::Connected => palette.green,
        Phase::Connecting | Phase::Failed => palette.warn,
        Phase::Disconnected => palette.dim,
    }
}

pub(super) fn target(profile: &ConnectionProfile) -> String {
    let endpoint = if profile.port == 22 {
        profile.host.clone()
    } else {
        format!("{}:{}", profile.host, profile.port)
    };
    if profile.user.is_empty() {
        endpoint
    } else {
        format!("{}@{endpoint}", profile.user)
    }
}

fn route_hops(profile: &ConnectionProfile) -> String {
    profile
        .jumps
        .iter()
        .map(|hop| hop.host.as_str())
        .collect::<Vec<_>>()
        .join(" → ")
}

fn file_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", bytes as f64 / 1_048_576.),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.),
    }
}

fn percent(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map_or_else(|| "—".into(), |value| format!("{value:.0}%"))
}

fn memory_size(used: u64, total: u64) -> String {
    let (unit, divisor) = if total >= 1_000_000_000_000 {
        ("T", 1_000_000_000_000.)
    } else {
        ("G", 1_000_000_000.)
    };
    let used = used as f64 / divisor;
    let total = total as f64 / divisor;
    if total < 100. {
        format!("{used:.1}/{total:.1}{unit}")
    } else {
        format!("{used:.0}/{total:.0}{unit}")
    }
}

fn metric(
    label: &'static str,
    value: String,
    fill: Option<f64>,
    palette: Palette,
    mono: SharedString,
) -> Div {
    let fill = fill
        .filter(|value| value.is_finite())
        .unwrap_or(0.)
        .clamp(0., 100.);
    col()
        .flex_1()
        .h(px(35.))
        .px(px(8.))
        .pt(px(5.))
        .pb(px(6.))
        .gap(px(6.))
        .bg(palette.base)
        .rounded(px(6.))
        .border_1()
        .border_color(palette.line)
        .child(
            row()
                .h(px(13.))
                .flex_shrink_0()
                .justify_between()
                .gap(px(4.))
                .font_family(mono)
                .text_size(px(11.))
                .line_height(px(13.))
                .child(div().flex_shrink_0().text_color(palette.muted).child(label))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                ),
        )
        .child(
            div()
                .h(px(3.))
                .flex_shrink_0()
                .rounded(px(2.))
                .bg(palette.line)
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(relative(fill as f32 / 100.))
                        .bg(palette.accent),
                ),
        )
}
