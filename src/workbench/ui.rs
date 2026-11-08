use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_kit::component::resizable::{HandleEdge, resize_handle};
use gpui_kit::component::{Disableable, Icon, Theme, ThemeMode, ThemeRegistry, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use super::{Drawer, Phase, SendTarget, Session, Tab, Workbench};
use crate::config::{ConnectionProfile, Settings};
use crate::local::LocalShell;
use crate::ssh::HopStage;

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) app: Hsla,
    pub(super) base: Hsla,
    pub(super) panel: Hsla,
    pub(super) raised: Hsla,
    pub(super) input: Hsla,
    pub(super) hover: Hsla,
    pub(super) line: Hsla,
    pub(super) line_strong: Hsla,
    pub(super) ink: Hsla,
    pub(super) muted: Hsla,
    pub(super) dim: Hsla,
    pub(super) accent: Hsla,
    pub(super) tint: Hsla,
    pub(super) green: Hsla,
    pub(super) warn: Hsla,
    pub(super) danger: Hsla,
    pub(super) route: Hsla,
    pub(super) primary_text: Hsla,
}

impl Palette {
    pub(super) fn new(light: bool) -> Self {
        let colors = if light {
            [
                0xE9EDF1, 0xFFFFFF, 0xF3F5F8, 0xFFFFFF, 0xFFFFFF, 0xE4E9EE, 0xD8DEE5, 0x17202A,
                0x4A5664, 0x65717F, 0x086F95, 0xD9EEF7, 0x157A4B, 0x8F5C00, 0xBE2E3A, 0xA62C95,
            ]
        } else {
            [
                0x0F1317, 0x0B0E12, 0x141920, 0x1B2129, 0x10151B, 0x1F262E, 0x232A33, 0xE4E9EE,
                0xA2AEBB, 0x7A8695, 0x5CC8E6, 0x16303B, 0x4CC38A, 0xE8B04A, 0xF0626A, 0xE07AE0,
            ]
        };
        let [
            app,
            base,
            panel,
            raised,
            input,
            hover,
            line,
            ink,
            muted,
            dim,
            accent,
            tint,
            green,
            warn,
            danger,
            route,
        ] = colors.map(|v| rgb(v).into());
        Self {
            app,
            base,
            panel,
            raised,
            input,
            hover,
            line,
            line_strong: rgb(if light { 0xBCC6D0 } else { 0x313B47 }).into(),
            ink,
            muted,
            dim,
            accent,
            tint,
            green,
            warn,
            danger,
            route,
            primary_text: rgb(if light { 0xFFFFFF } else { 0x06222C }).into(),
        }
    }
}

pub(super) fn install_theme(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    registry
        .load_themes_from_str(include_str!("../../design/theme/capterm.theme.json"))
        .expect("valid bundled CapTerm themes");
    let dark = registry.themes()["CapTerm Night"].clone();
    let light = registry.themes()["CapTerm Day"].clone();
    let theme = Theme::global_mut(cx);
    theme.dark_theme = dark;
    theme.light_theme = light;
    for font in [
        include_bytes!("../../assets/fonts/NotoSansSC.ttf").as_slice(),
        include_bytes!("../../assets/fonts/MapleMono-NF-CN-Regular.ttf").as_slice(),
    ] {
        if let Err(error) = cx
            .text_system()
            .add_fonts(vec![std::borrow::Cow::Borrowed(font)])
        {
            eprintln!("Unable to register CapTerm font: {error}");
        }
    }
}

pub(super) fn apply_theme(settings: &Settings, cx: &mut App) {
    Theme::change(
        if settings.light {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        },
        None,
        cx,
    );
    if settings.ui_font_family.is_some() || settings.mono_font_family.is_some() {
        let fonts = cx.text_system().all_font_names();
        let available = |requested: Option<&str>| {
            requested.and_then(|name| fonts.iter().find(|font| font.eq_ignore_ascii_case(name)))
        };
        Theme::update(cx, |theme| {
            if let Some(family) = available(settings.ui_font_family.as_deref()) {
                theme.font_family = family.clone().into();
            }
            if let Some(family) = available(settings.mono_font_family.as_deref()) {
                theme.mono_font_family = family.clone().into();
            }
        });
    }
}

pub(super) fn row() -> Div {
    div().flex().flex_row().items_center().min_w_0()
}
pub(super) fn col() -> Div {
    div().flex().flex_col().min_w_0().min_h_0()
}

pub(super) fn input(state: &Entity<InputState>, label: &'static str, p: Palette) -> Input {
    Input::new(state)
        .aria_label(label)
        .h(px(28.))
        .w_full()
        .min_w_0()
        .bg(p.input)
        .border_color(p.line)
        .rounded(px(6.))
}

pub(super) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    p: Palette,
) -> Button {
    Button::new(id)
        .ghost()
        .label(label)
        .h(px(31.))
        .px(px(9.))
        .rounded(px(6.))
        .border_1()
        .border_color(p.line)
        .text_size(px(12.))
}

pub(super) fn primary(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    p: Palette,
) -> Button {
    button(id, label, p)
        .primary()
        .border_color(p.accent)
        .font_weight(FontWeight::SEMIBOLD)
}

pub(super) fn tool(
    id: impl Into<ElementId>,
    glyph: impl Into<SharedString>,
    title: &'static str,
    p: Palette,
) -> Button {
    Button::new(id)
        .ghost()
        .label(glyph)
        .accessibility_label(title)
        .tooltip(title)
        .h(px(26.))
        .min_w(px(26.))
        .px(px(5.))
        .rounded(px(6.))
        .text_size(px(13.))
        .text_color(p.muted)
}

pub(super) fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

fn app_icon() -> Arc<Image> {
    static ICON: OnceLock<Arc<Image>> = OnceLock::new();
    ICON.get_or_init(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Ico,
            include_bytes!("../../design/icon/capterm.ico").to_vec(),
        ))
    })
    .clone()
}

pub(super) fn icon_tool(
    id: impl Into<ElementId>,
    name: IconName,
    title: &'static str,
    p: Palette,
) -> Button {
    sized_icon_tool(id, name, title, 14., 24., p)
}

pub(super) fn chrome_icon_tool(
    id: impl Into<ElementId>,
    name: IconName,
    title: &'static str,
    p: Palette,
) -> Button {
    sized_icon_tool(id, name, title, 16., 28., p)
}

pub(super) fn tab_close_tool(
    id: impl Into<ElementId>,
    name: IconName,
    title: &'static str,
    p: Palette,
) -> Button {
    sized_icon_tool(id, name, title, 12., 20., p).rounded(px(4.))
}

fn sized_icon_tool(
    id: impl Into<ElementId>,
    name: IconName,
    title: &'static str,
    glyph_size: f32,
    hit_size: f32,
    p: Palette,
) -> Button {
    // Button::icon applies the button's sizing preset to the glyph. Keep the
    // Lucide glyph at the design size independently of its hit target.
    Button::new(id)
        .ghost()
        .child(Icon::new(name).size(px(glyph_size)))
        .accessibility_label(title)
        .tooltip(title)
        .size(px(hit_size))
        .min_w(px(hit_size))
        .flex_shrink_0()
        .p_0()
        .rounded(px(6.))
        .text_color(p.muted)
}

pub(super) fn pane_head(p: Palette) -> Div {
    row()
        .h(px(34.))
        .flex_shrink_0()
        .justify_between()
        .gap(px(6.))
        .px(px(11.))
        .border_b_1()
        .border_color(p.line)
        .font_weight(FontWeight::SEMIBOLD)
}

pub(super) fn badge(label: &'static str, p: Palette, mono: SharedString) -> Div {
    div()
        .border_1()
        .border_color(p.line)
        .rounded(px(4.))
        .px(px(5.))
        .font_family(mono)
        .text_size(px(11.))
        .text_color(p.muted)
        .child(label)
}

pub(super) fn muted_message(message: impl Into<SharedString>, p: Palette) -> Div {
    div()
        .px(px(12.))
        .py(px(11.))
        .text_size(px(12.))
        .text_color(p.muted)
        .child(message.into())
}

pub(super) fn menu_action(
    label: impl Into<SharedString>,
    view: &WeakEntity<Workbench>,
    action: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
) -> PopupMenuItem {
    let view = view.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        let _ = view.update(cx, |this, cx| action(this, window, cx));
    })
}

#[derive(Clone, Copy)]
enum Split {
    Files,
    Sessions,
    Monitor,
}
struct SplitDrag;
impl Render for SplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        let compact = size.width < px(900.);
        let narrow_right = size.width < px(1200.);
        if compact != self.compact {
            self.compact = compact;
            for tab in &self.tabs {
                tab.terminal
                    .update(cx, |terminal, cx| terminal.set_compact(compact, cx));
            }
        }
        let p = Palette::new(self.settings.light);
        let mono = Theme::global(cx).mono_font_family.clone();
        let font = Theme::global(cx).font_family.clone();
        let full_left = !self.focus_mode && !compact && self.left_visible;
        let full_right = !self.focus_mode && !narrow_right && self.right_visible;
        let rail = !self.focus_mode && narrow_right && self.right_visible;
        let right_reserve = if full_right {
            self.right_width.min(px(340.))
        } else if rail {
            px(44.)
        } else {
            px(0.)
        };
        let max_left = (size.width - right_reserve - px(340.)).clamp(px(220.), px(480.));
        let left = self.left_width.min(max_left);
        let left_reserve = if full_left { left } else { px(0.) };
        let right = self
            .right_width
            .min((size.width - left_reserve - px(340.)).clamp(px(260.), px(440.)));
        let monitor_height = self
            .monitor_height
            .min((size.height - px(184.)).max(px(130.)));
        let center_width = size.width
            - if full_left { left } else { px(0.) }
            - if full_right {
                right
            } else if rail {
                px(44.)
            } else {
                px(0.)
            };
        let mut workspace = row()
            .items_stretch()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden();
        if full_left {
            workspace = workspace.child(
                div()
                    .relative()
                    .flex()
                    .items_stretch()
                    .h_full()
                    .w(left)
                    .flex_shrink_0()
                    .bg(p.panel)
                    .child(self.render_files(left, p, mono.clone(), window, cx))
                    .child(self.splitter(Split::Files, cx)),
            );
        }
        workspace = workspace.child(self.middle(p, mono.clone(), center_width, window, cx));
        if full_right {
            let monitor = self.host_monitor(p, mono.clone(), monitor_height, window, cx);
            workspace = workspace.child(
                col()
                    .relative()
                    .h_full()
                    .w(right)
                    .flex_shrink_0()
                    .bg(p.panel)
                    .child(self.session_panel(p, mono.clone(), cx))
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .child(monitor)
                            .child(self.splitter(Split::Monitor, cx)),
                    )
                    .child(self.splitter(Split::Sessions, cx)),
            );
        }
        if rail {
            workspace = workspace.child(self.rail(p, cx));
        }
        let drawer = match self.drawer {
            Some(Drawer::Files) if !self.focus_mode && compact => {
                let width = px(272.).min(size.width - px(52.));
                Some((true, self.render_files(width, p, mono.clone(), window, cx)))
            }
            Some(Drawer::Sessions) if rail => Some((
                false,
                self.session_panel(p, mono.clone(), cx)
                    .w(px(296.))
                    .h_full()
                    .into_any_element(),
            )),
            Some(Drawer::Monitor) if rail => Some((
                false,
                self.host_monitor(p, mono.clone(), monitor_height, window, cx)
                    .w(px(296.))
                    .into_any_element(),
            )),
            _ => None,
        };
        if let Some((left, panel)) = drawer {
            workspace = workspace
                .child(
                    div()
                        .id("close-panel-drawer")
                        .absolute()
                        .inset_0()
                        .when(rail, |el| el.right(px(44.)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.drawer = None;
                            cx.notify();
                        })),
                )
                .child(
                    col()
                        .absolute()
                        .top_0()
                        .when(left, |el| el.left_0().bottom_0())
                        .when(!left, |el| el.right(px(44.)))
                        .when(!left && self.drawer == Some(Drawer::Sessions), |el| {
                            el.bottom_0()
                        })
                        .shadow_lg()
                        .occlude()
                        .border_1()
                        .border_color(p.line)
                        .bg(p.panel)
                        .child(panel),
                );
        }
        col()
            .relative()
            .size_full()
            .font_family(font)
            .text_size(px(13.))
            .text_color(p.ink)
            .bg(p.app)
            .when(!self.focus_mode, |root| {
                root.child(self.top(p, mono.clone(), size.width, cx))
            })
            .child(workspace)
            .when(!self.focus_mode, |root| {
                root.child(self.status(p, mono, size.width, cx))
            })
            .when(self.notifications_open, |root| {
                root.child(self.notification_center(p, cx))
            })
    }
}

impl Workbench {
    fn splitter(&self, split: Split, cx: &Context<Self>) -> impl IntoElement {
        let (id, axis, edge) = match split {
            Split::Files => ("resize-files", Axis::Horizontal, HandleEdge::Trailing),
            Split::Sessions => ("resize-sessions", Axis::Horizontal, HandleEdge::Leading),
            Split::Monitor => ("resize-monitor", Axis::Vertical, HandleEdge::Leading),
        };
        resize_handle(id, axis)
            .inside(edge)
            .on_drag(split, |_, _, _, cx| cx.new(|_| SplitDrag))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<Split>, window, cx| {
                    let viewport = window.viewport_size();
                    let position = event.event.position;
                    match event.drag(cx) {
                        Split::Files => this.left_width = position.x.clamp(px(220.), px(480.)),
                        Split::Sessions => {
                            this.right_width =
                                (viewport.width - position.x).clamp(px(260.), px(440.))
                        }
                        Split::Monitor => {
                            this.monitor_height = (viewport.height - px(24.) - position.y)
                                .clamp(px(130.), (viewport.height - px(184.)).max(px(130.)))
                        }
                    }
                    cx.notify();
                }),
            )
    }

    fn top(&self, p: Palette, mono: SharedString, width: Pixels, cx: &Context<Self>) -> TitleBar {
        let shortcuts = if cfg!(target_os = "macos") {
            &["⌘", "P"][..]
        } else {
            &["Ctrl", "Shift", "P"][..]
        };
        let logo = img(app_icon()).size(px(22.)).flex_shrink_0();
        TitleBar::new()
            .h(px(40.))
            .bg(p.app)
            .border_color(p.line)
            .child(
                row()
                    .gap(px(8.))
                    .flex_1()
                    .child(logo)
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .child("CapTerm"),
                    )
                    .when(width >= px(960.), |bar| {
                        bar.child(
                            div()
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child("默认工作区"),
                        )
                    }),
            )
            .child(
                row()
                    .id("command-palette-trigger")
                    // Keep the title bar's native caption hitbox behind app controls.
                    .occlude()
                    .h(px(29.))
                    .flex_1()
                    .max_w(px(462.))
                    .mx(px(8.))
                    .gap(px(8.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.input)
                    .cursor_pointer()
                    .tab_stop(true)
                    .hover(|s| s.bg(p.hover))
                    .child(icon(IconName::Search, 14., p.dim))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(p.muted)
                            .text_size(px(13.))
                            .child("搜索会话、命令，或输入 user@host 快速连接"),
                    )
                    .when(width >= px(800.), |el| {
                        el.child(row().gap(px(3.)).font_family(mono).children(
                            shortcuts.iter().map(|key| {
                                div()
                                    .text_size(px(10.))
                                    .px(px(4.))
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(p.line)
                                    .bg(p.raised)
                                    .text_color(p.muted)
                                    .child(*key)
                            }),
                        ))
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.command_palette(window, cx))),
            )
            .child(
                row()
                    .flex_1()
                    .justify_end()
                    .min_w(px(170.))
                    .pr(px(9.))
                    .child(
                        row()
                            .id("titlebar-actions")
                            .occlude()
                            .flex_shrink_0()
                            .gap(px(4.))
                            .child(
                                chrome_icon_tool(
                                    "toggle-files",
                                    IconName::PanelLeft,
                                    "文件面板 · Ctrl+Shift+E",
                                    p,
                                )
                                .when(
                                    self.left_visible && !self.compact
                                        || self.drawer == Some(Drawer::Files),
                                    |el| el.text_color(p.accent),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_left(cx))),
                            )
                            .child(
                                chrome_icon_tool(
                                    "toggle-sessions",
                                    IconName::PanelRight,
                                    "会话与监控 · Ctrl+Shift+B",
                                    p,
                                )
                                .when(self.right_visible, |el| el.text_color(p.accent))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_right(cx))),
                            )
                            .child(div().h(px(16.)).w(px(1.)).bg(p.line))
                            .child(
                                chrome_icon_tool(
                                    "theme-toggle",
                                    if self.settings.light {
                                        IconName::Moon
                                    } else {
                                        IconName::Sun
                                    },
                                    "切换主题 · Ctrl+Shift+L",
                                    p,
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.toggle_theme(window, cx)
                                    }),
                                ),
                            )
                            .child(
                                div()
                                    .relative()
                                    .child(
                                        chrome_icon_tool(
                                            "open-notifications",
                                            IconName::Bell,
                                            "通知",
                                            p,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.notifications_open = !this.notifications_open;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .when(self.notifications.iter().any(|n| !n.read), |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .top(px(3.))
                                                .right(px(4.))
                                                .size(px(5.))
                                                .rounded_full()
                                                .bg(p.accent),
                                        )
                                    }),
                            )
                            .child(
                                chrome_icon_tool(
                                    "open-settings",
                                    IconName::Settings,
                                    "设置 · Ctrl+,",
                                    p,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.settings_dialog(window, cx),
                                )),
                            ),
                    ),
            )
    }

    fn rail(&self, p: Palette, cx: &Context<Self>) -> Div {
        col()
            .w(px(44.))
            .items_center()
            .pt(px(8.))
            .gap(px(4.))
            .flex_shrink_0()
            .bg(p.panel)
            .border_l_1()
            .border_color(p.line)
            .child(
                chrome_icon_tool("rail-sessions", IconName::Server, "会话", p)
                    .when(self.drawer == Some(Drawer::Sessions), |el| {
                        el.bg(p.tint).text_color(p.accent)
                    })
                    .on_click(
                        cx.listener(|this, _, _, cx| this.toggle_drawer(Drawer::Sessions, cx)),
                    ),
            )
            .child(
                chrome_icon_tool("rail-monitor", IconName::Activity, "主机监控", p)
                    .when(self.drawer == Some(Drawer::Monitor), |el| {
                        el.bg(p.tint).text_color(p.accent)
                    })
                    .on_click(
                        cx.listener(|this, _, _, cx| this.toggle_drawer(Drawer::Monitor, cx)),
                    ),
            )
            .child(div().w(px(20.)).h(px(1.)).my(px(4.)).bg(p.line))
            .child(
                chrome_icon_tool("rail-import", IconName::Import, "导入 SSH 配置", p)
                    .on_click(cx.listener(|this, _, window, cx| this.import_dialog(window, cx))),
            )
    }

    fn toggle_drawer(&mut self, drawer: Drawer, cx: &mut Context<Self>) {
        self.drawer = (self.drawer != Some(drawer)).then_some(drawer);
        cx.notify();
    }

    fn middle(
        &self,
        p: Palette,
        mono: SharedString,
        width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let weak = cx.weak_entity();
        let tabs = row()
            .id("workspace-tabs")
            .h_full()
            .flex_1()
            .items_stretch()
            .overflow_x_scroll()
            .children(self.tabs.iter().map(|tab| self.session_tab(tab, p, cx)))
            .children(self.render_file_editor_tabs(p, cx))
            .child(row().px(px(3.)).child(
                chrome_icon_tool("open-new-tab", IconName::Plus, "新标签", p).dropdown_menu(
                    move |menu, _, _| {
                        let menu = menu
                            .item(menu_action(
                                "搜索会话 / 快速连接",
                                &weak,
                                |view, window, cx| view.command_palette(window, cx),
                            ))
                            .item(menu_action("新建会话", &weak, |view, window, cx| {
                                view.profile_dialog(None, window, cx)
                            }));
                        if !cfg!(windows) {
                            return menu;
                        }
                        [
                            LocalShell::PowerShell,
                            LocalShell::CommandPrompt,
                            LocalShell::Wsl,
                        ]
                        .into_iter()
                        .fold(menu.separator(), |menu, shell| {
                            menu.item(menu_action(
                                shell.label(),
                                &weak,
                                move |view, window, cx| view.launch_local(shell, window, cx),
                            ))
                        })
                    },
                ),
            ));
        let middle = col().flex_1().overflow_hidden().bg(p.base).child(
            row()
                .h(px(34.))
                .flex_shrink_0()
                .bg(p.app)
                .border_b_1()
                .border_color(p.line)
                .child(tabs)
                .child(
                    row()
                        .px(px(5.))
                        .gap(px(2.))
                        .child(
                            icon_tool(
                                "focus-mode",
                                if self.focus_mode {
                                    IconName::Minimize
                                } else {
                                    IconName::Maximize
                                },
                                "专注模式 · Ctrl+Shift+Enter",
                                p,
                            )
                            .when(self.focus_mode, |el| el.bg(p.tint))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_focus_mode(cx))),
                        )
                        .child(self.tab_switcher(p, cx)),
                ),
        );
        let tab = self.active_tab();
        let surface = if let Some(editor) = self.render_file_editor(p, mono.clone(), window, cx) {
            editor
        } else {
            let terminal = col()
                .size_full()
                .relative()
                .overflow_hidden()
                .when_some(tab, |el, tab| el.child(tab.terminal.clone()))
                .when(tab.is_none(), |el| el.child(self.empty_terminal(p, cx)));
            self.file_input_drop("terminal-drop", terminal, p, cx)
                .flex_1()
                .into_any_element()
        };
        middle
            .when(!self.focus_mode, |el| {
                el.child(self.route_bar(p, mono.clone(), cx))
            })
            .child(surface)
            .when_some(
                tab.filter(|tab| self.active_file_editor.is_none() && tab.can_reconnect()),
                |el, tab| {
                    let id = tab.id;
                    let hint = if tab.phase == Phase::Failed {
                        "SSH 连接失败 · 在终端中按 Enter 重连"
                    } else {
                        "SSH 已断开 · 在终端中按 Enter 重连"
                    };
                    el.child(
                        row()
                            .flex_shrink_0()
                            .px(px(10.))
                            .py(px(4.))
                            .gap(px(8.))
                            .bg(p.panel)
                            .border_t_1()
                            .border_color(p.line)
                            .child(status_dot(tab.phase, p, 7.))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(12.))
                                    .text_color(p.ink)
                                    .child(hint),
                            )
                            .child(
                                button(("retry-ssh", id), "重新连接", p).on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.request_reconnect(id, window, cx)
                                    },
                                )),
                            ),
                    )
                },
            )
            .child(
                self.file_input_drop("sender-drop", self.sender(p, mono, width, cx), p, cx)
                    .flex_shrink_0(),
            )
    }

    fn tab_switcher(&self, p: Palette, cx: &Context<Self>) -> impl IntoElement {
        let tabs = self
            .tabs
            .iter()
            .map(|tab| (tab.id, tab.name().to_owned()))
            .collect::<Vec<_>>();
        let weak = cx.weak_entity();
        icon_tool("tab-switcher", IconName::ChevronDown, "切换终端标签", p)
            .disabled(tabs.is_empty())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                tabs.iter().fold(menu, |menu, (id, name)| {
                    let id = *id;
                    menu.item(menu_action(name.clone(), &weak, move |v, w, c| {
                        v.select_tab(id, w, c)
                    }))
                })
            })
    }

    fn empty_terminal(&self, p: Palette, cx: &Context<Self>) -> Div {
        col()
            .size_full()
            .justify_center()
            .items_center()
            .gap(px(11.))
            .px(px(16.))
            .child(
                row()
                    .size(px(42.))
                    .justify_center()
                    .bg(p.tint)
                    .rounded(px(8.))
                    .child(icon(IconName::Terminal, 21., p.accent)),
            )
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("打开一个 SSH 会话"),
            )
            .child(
                div()
                    .text_center()
                    .text_size(px(12.))
                    .text_color(p.dim)
                    .child("从会话列表打开连接，或在命令面板输入 user@host。"),
            )
            .child(
                row()
                    .gap(px(8.))
                    .child(primary("empty-open", "打开命令面板", p).on_click(
                        cx.listener(|this, _, window, cx| this.command_palette(window, cx)),
                    ))
                    .child(button("empty-new", "新建会话", p).on_click(
                        cx.listener(|this, _, window, cx| this.profile_dialog(None, window, cx)),
                    )),
            )
    }

    fn session_tab(&self, tab: &Tab, p: Palette, cx: &Context<Self>) -> Div {
        let id = tab.id;
        let selected = self.active == Some(id) && self.active_file_editor.is_none();
        let agent = tab.observed_terminal.agent_running;
        row()
            .h_full()
            .relative()
            .gap(px(3.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(p.line)
            .bg(if selected { p.base } else { p.app })
            .when(selected, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(p.accent),
                )
            })
            .child(
                row()
                    .id(("session-tab", id))
                    .min_w(px(72.))
                    .max_w(px(245.))
                    .h_full()
                    .gap(px(7.))
                    .pl(px(11.))
                    .pr(px(4.))
                    .cursor_pointer()
                    .tab_stop(true)
                    .text_color(if selected { p.ink } else { p.muted })
                    .hover(|style| style.bg(p.hover))
                    .child(status_dot(tab.phase, p, 7.))
                    .child(div().truncate().child(tab.name().to_owned()))
                    .when(agent, |el| el.child(icon(IconName::Bot, 12., p.muted)))
                    .when(tab.pending_notification, |el| {
                        el.child(
                            row()
                                .gap(px(3.))
                                .rounded_full()
                                .bg(p.tint)
                                .text_color(p.accent)
                                .text_size(px(10.))
                                .px(px(4.))
                                .child(icon(IconName::BellRing, 11., p.accent))
                                .child("待确认"),
                        )
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(id, window, cx)),
                    ),
            )
            .child(
                tab_close_tool(("close-session-tab", id), IconName::X, "关闭标签", p)
                    .mr(px(5.))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.close_tab(id, window, cx)),
                    ),
            )
    }

    fn route_bar(&self, p: Palette, mono: SharedString, cx: &Context<Self>) -> Div {
        let tab = self.active_tab();
        let active = self.active;
        let raw = tab.is_some_and(|tab| tab.terminal.read(cx).raw());
        let mut nodes = row()
            .flex_1()
            .overflow_hidden()
            .gap(px(7.))
            .text_size(px(12.))
            .child(icon(IconName::Laptop, 14., p.dim))
            .child(div().flex_shrink_0().text_color(p.dim).child("本机"));
        if let Some(tab) = tab {
            match &tab.session {
                Session::Local(shell) => {
                    nodes = nodes
                        .child(div().text_color(p.dim).child("—"))
                        .child(div().font_family(mono.clone()).child(shell.label()))
                }
                Session::Ssh(profile) => {
                    for (index, (label, endpoint, is_target)) in profile
                        .jumps
                        .iter()
                        .map(|hop| {
                            (
                                hop.host.as_str(),
                                crate::config::ssh_address(&hop.user, &hop.host, Some(hop.port)),
                                false,
                            )
                        })
                        .chain(std::iter::once((
                            profile.name.as_str(),
                            crate::config::ssh_address(
                                &profile.user,
                                &profile.host,
                                Some(profile.port),
                            ),
                            true,
                        )))
                        .enumerate()
                    {
                        let progress = tab.route.iter().find(|hop| hop.index == index);
                        let live_rtt = tab
                            .sample
                            .as_ref()
                            .and_then(|metrics| metrics.hop_rtt_ms.get(index).copied().flatten());
                        let stage = progress.map(|hop| hop.stage);
                        let color = match stage {
                            Some(HopStage::Connected) if tab.phase != Phase::Disconnected => {
                                p.route
                            }
                            Some(HopStage::Failed) => p.danger,
                            Some(
                                HopStage::Connecting
                                | HopStage::ProxyApproval
                                | HopStage::Trust
                                | HopStage::Authenticating,
                            ) => p.warn,
                            _ => p.dim,
                        };
                        let rtt = live_rtt.or_else(|| progress.and_then(|hop| hop.rtt_ms));
                        let state = stage.map(route_stage_label).unwrap_or("尚未连接");
                        let pending_stage = is_target
                            .then(|| {
                                tab.route
                                    .iter()
                                    .find(|hop| hop.stage != HopStage::Connected)
                                    .map(|hop| hop.stage)
                            })
                            .flatten();
                        let auth = progress.and_then(|hop| hop.authentication).unwrap_or("SSH");
                        let error = progress.and_then(|hop| hop.error.as_deref()).unwrap_or("");
                        let mut tooltip = format!("{endpoint} · {state} · {auth} {error}");
                        if let Some(cipher) = progress.and_then(|hop| hop.cipher.as_deref()) {
                            tooltip.push_str(&format!(" · {cipher}"));
                        }
                        if is_target && let Some(title) = tab.terminal.read(cx).title() {
                            tooltip.push_str(&format!("\n{title}"));
                        }
                        let line = |id| route_segment(id, stage, color);
                        nodes = nodes.child(
                            row()
                                .gap(px(5.))
                                .flex_shrink_0()
                                .child(line(("route-a", index)))
                                .when_some(rtt, |el, rtt| {
                                    el.child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(p.muted)
                                            .child(format!("{rtt} ms")),
                                    )
                                    .child(line(("route-b", index)))
                                })
                                .child(
                                    Button::new(("route-hop", index))
                                        .ghost()
                                        .h(px(24.))
                                        .px(px(2.))
                                        .min_w_0()
                                        .rounded(px(4.))
                                        .tooltip(tooltip)
                                        .child(
                                            row()
                                                .gap(px(5.))
                                                .child(if is_target {
                                                    row()
                                                        .size(px(11.))
                                                        .border_2()
                                                        .border_color(color)
                                                        .rounded_full()
                                                        .justify_center()
                                                        .child(
                                                            div()
                                                                .size(px(3.))
                                                                .rounded_full()
                                                                .bg(color),
                                                        )
                                                } else {
                                                    div().size(px(8.)).rounded_full().bg(color)
                                                })
                                                .child(
                                                    div()
                                                        .font_family(mono.clone())
                                                        .text_size(px(12.))
                                                        .font_weight(if is_target {
                                                            FontWeight::SEMIBOLD
                                                        } else {
                                                            FontWeight::NORMAL
                                                        })
                                                        .text_color(if is_target {
                                                            p.ink
                                                        } else {
                                                            p.muted
                                                        })
                                                        .child(label.to_owned()),
                                                ),
                                        ),
                                )
                                .when_some(pending_stage, |el, stage| {
                                    let color = if stage == HopStage::Failed {
                                        p.danger
                                    } else {
                                        p.warn
                                    };
                                    el.child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(color)
                                            .child(route_stage_label(stage)),
                                    )
                                }),
                        );
                    }
                }
            }
        } else {
            nodes = nodes.child(div().text_color(p.dim).child("─  等待连接"));
        }
        let agent = tab.and_then(|tab| tab.terminal.read(cx).agent_name());
        let tui = tab.and_then(|tab| tab.terminal.read(cx).fullscreen_name());
        let agent_duration = tab
            .and_then(|tab| tab.terminal.read(cx).agent_elapsed())
            .map(|elapsed| elapsed.as_secs() / 60)
            .unwrap_or(0);
        let weak = cx.weak_entity();
        row()
            .h(px(32.))
            .flex_shrink_0()
            .justify_between()
            .gap(px(7.))
            .px(px(9.))
            .border_b_1()
            .border_color(p.line)
            .bg(p.base)
            .child(nodes)
            .when_some(agent.or(tui), |el, name| {
                el.child(
                    row()
                        .gap(px(5.))
                        .rounded_full()
                        .px(px(6.))
                        .h(px(20.))
                        .bg(p.tint)
                        .text_size(px(11.))
                        .text_color(p.accent)
                        .child(icon(
                            if tui.is_some() {
                                IconName::AppWindow
                            } else {
                                IconName::Bot
                            },
                            12.,
                            p.accent,
                        ))
                        .child(if tui.is_some() {
                            format!("全屏 · {name}")
                        } else if agent_duration > 0 {
                            format!("{name} 运行 {agent_duration} 分钟")
                        } else {
                            name.to_owned()
                        }),
                )
            })
            .when(self.active_file_editor.is_none(), |bar| {
                bar.child(
                    row()
                        .gap(px(2.))
                        .flex_shrink_0()
                        .child(
                            icon_tool("route-line-numbers", IconName::Hash, "显示行号", p)
                                .disabled(tab.is_none())
                                .when(tab.is_some_and(|t| t.line_numbers), |el| {
                                    el.bg(p.tint).text_color(p.accent)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_gutters(Some(true), cx)
                                })),
                        )
                        .child(
                            icon_tool("route-timestamps", IconName::Clock, "显示时间戳", p)
                                .disabled(tab.is_none() || raw)
                                .when(!raw && tab.is_some_and(|t| t.timestamps), |el| {
                                    el.bg(p.tint).text_color(p.accent)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_gutters(Some(false), cx)
                                })),
                        )
                        .child(
                            icon_tool(
                                "route-search",
                                IconName::ListFilter,
                                "搜索终端 · Ctrl+Shift+F",
                                p,
                            )
                            .disabled(tab.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.search_terminal(window, cx)),
                            ),
                        )
                        .child(
                            icon_tool("route-actions", IconName::EllipsisVertical, "终端操作", p)
                                .disabled(tab.is_none())
                                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                    let Some(id) = active else { return menu };
                                    menu.item(menu_action(
                                        "复制 SSH 命令",
                                        &weak,
                                        move |view, _, cx| {
                                            if let Some(profile) =
                                                view.tab(id).and_then(Tab::ssh_profile)
                                            {
                                                let command = profile.ssh_command();
                                                if command.is_empty() {
                                                    view.announce(
                                                        "此代理设置无法直接转换为 OpenSSH 命令",
                                                        cx,
                                                    );
                                                } else {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(command),
                                                    );
                                                }
                                            }
                                        },
                                    ))
                                    .item(menu_action(
                                        "复制日志（含行号与时间）",
                                        &weak,
                                        move |view, _, cx| {
                                            if let Some(tab) = view.tab(id) {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    tab.terminal.read(cx).formatted_log(),
                                                ));
                                            }
                                        },
                                    ))
                                    .item(menu_action(
                                        "导出会话日志…",
                                        &weak,
                                        move |view, window, cx| {
                                            view.export_terminal(id, window, cx)
                                        },
                                    ))
                                    .item(menu_action(
                                        if raw {
                                            "恢复终端时间线"
                                        } else {
                                            "直接键盘输入模式"
                                        },
                                        &weak,
                                        move |view, _, cx| {
                                            if let Some(tab) = view.tab(id) {
                                                tab.terminal.update(cx, |terminal, cx| {
                                                    terminal.set_raw(!raw, cx)
                                                });
                                            }
                                        },
                                    ))
                                    .separator()
                                    .item(menu_action(
                                        "重新连接",
                                        &weak,
                                        move |view, window, cx| {
                                            if view.tab(id).is_some_and(Tab::can_reconnect) {
                                                view.request_reconnect(id, window, cx);
                                                return;
                                            }
                                            let session =
                                                view.tab(id).map(|tab| match &tab.session {
                                                    Session::Ssh(profile) => {
                                                        Session::Ssh(profile.clone())
                                                    }
                                                    Session::Local(shell) => Session::Local(*shell),
                                                });
                                            if let Some(session) = session {
                                                view.close_tab(id, window, cx);
                                                match session {
                                                    Session::Ssh(profile) => {
                                                        view.connect_new(profile, None, window, cx)
                                                    }
                                                    Session::Local(shell) => {
                                                        view.launch_local(shell, window, cx)
                                                    }
                                                }
                                            }
                                        },
                                    ))
                                    .item(menu_action(
                                        "断开连接",
                                        &weak,
                                        move |view, _, _| {
                                            if let Some(tab) = view.tab(id) {
                                                match tab.link.commands.borrow().as_ref() {
                                                    Some(super::ShellInput::Ssh(sender)) => {
                                                        let _ = sender
                                                            .send(crate::ssh::ShellCommand::Close);
                                                    }
                                                    Some(super::ShellInput::Local(sender)) => {
                                                        let _ = sender
                                                            .send(crate::ssh::ShellCommand::Close);
                                                    }
                                                    None => {}
                                                }
                                            }
                                        },
                                    ))
                                }),
                        ),
                )
            })
    }

    fn sender(&self, p: Palette, mono: SharedString, width: Pixels, cx: &Context<Self>) -> Div {
        let tab = self.active_tab();
        let active = self.active;
        let draft = tab.map_or(&self.empty_draft, |t| &t.draft);
        let raw = tab.is_some_and(|tab| tab.terminal.read(cx).raw());
        let name = tab.map(Tab::name).unwrap_or("当前会话");
        if let Some(tab) = tab
            && raw
            && !tab.show_sender
        {
            let name = tab
                .terminal
                .read(cx)
                .fullscreen_name()
                .unwrap_or("全屏程序");
            return row()
                .h(px(34.))
                .justify_between()
                .px(px(9.))
                .bg(p.panel)
                .border_t_1()
                .border_color(p.line)
                .child(
                    row()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child(icon(IconName::Keyboard, 15., p.accent))
                        .child(format!("键盘输入直接发送给 {name}")),
                )
                .child(
                    tool(
                        "expand-sender",
                        "展开输入栏",
                        "聚焦输入栏 · Ctrl+Shift+I",
                        p,
                    )
                    .on_click(cx.listener(|view, _, window, cx| view.focus_sender(window, cx))),
                );
        }
        let agent = tab.and_then(|tab| tab.terminal.read(cx).agent_name());
        let paste = tab.and_then(|t| t.paste_mode).unwrap_or(agent.is_some());
        let count = active.map_or(0, |id| self.draft_receivers(id).len());
        let broadcast = !matches!(self.send_target, SendTarget::Active);
        let label = match self.send_target {
            SendTarget::Active if width >= px(680.) => format!("发送到 {name}"),
            SendTarget::Active => "发送目标".into(),
            SendTarget::All => format!("广播 · {count} 个"),
            SendTarget::Selected(_) => format!("已选 · {count} 个"),
        };
        let narrow = width < px(650.);
        let weak = cx.weak_entity();
        let select = button("sender-target", label, p)
            .max_w(px(if narrow { 104. } else { 178. }))
            .icon(icon(
                IconName::SquareTerminal,
                13.,
                if broadcast { p.warn } else { p.muted },
            ))
            .when(broadcast, |button| {
                button.text_color(p.warn).border_color(p.warn.opacity(0.65))
            })
            .dropdown_menu(move |menu, _, _| {
                menu.item(menu_action("当前会话", &weak, |this, _, cx| {
                    this.send_target = SendTarget::Active;
                    cx.notify();
                }))
                .item(menu_action(
                    "广播到所有已连接会话",
                    &weak,
                    |this, _, cx| {
                        this.send_target = SendTarget::All;
                        cx.notify();
                    },
                ))
                .item(menu_action(
                    "选择会话…",
                    &weak,
                    |this, window, cx| this.choose_targets(window, cx),
                ))
            });
        let mode_buttons = [
            ("sender-lines", false, "逐行执行"),
            ("sender-paste", true, "整段粘贴"),
        ];
        let active_connected = tab.is_some_and(|t| t.phase == Phase::Connected);
        let sender_view = cx.weak_entity();
        let source = row()
            .flex_1()
            .min_w_0()
            .items_end()
            .gap(px(3.))
            .px(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(p.line)
            .bg(p.input)
            .child(
                div()
                    .h(px(30.))
                    .flex_shrink_0()
                    .pl(px(4.))
                    .flex()
                    .items_center()
                    .child(icon(IconName::ChevronRight, 16., p.accent)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Textarea::new(draft)
                        .aria_label("命令输入")
                        .on_paste(move |clipboard, _, cx| {
                            let Some(id) = active else { return false };
                            if !active_connected || (clipboard.text().is_some() && agent.is_none())
                            {
                                return false;
                            }
                            let Some(image) =
                                clipboard.entries.iter().find_map(|entry| match entry {
                                    ClipboardEntry::Image(image) if !image.bytes.is_empty() => {
                                        Some(image)
                                    }
                                    _ => None,
                                })
                            else {
                                return false;
                            };
                            sender_view
                                .update(cx, |view, cx| {
                                    view.upload_terminal_clipboard(
                                        id,
                                        image.bytes.clone(),
                                        image.format.extension(),
                                        cx,
                                    );
                                })
                                .is_ok()
                        })
                        .font_family(mono)
                        .text_size(px(13.))
                        .line_height(px(19.))
                        .min_h(px(30.))
                        .appearance(false)
                        .bordered(false)
                        .disabled(!active_connected),
                ),
            )
            .when(!narrow, |el| {
                el.child(
                    icon_tool("sender-snippets", IconName::ScrollText, "命令片段", p)
                        .mb(px(2.))
                        .on_click(
                            cx.listener(|view, _, window, cx| view.snippet_picker(window, cx)),
                        ),
                )
                .child(
                    icon_tool("sender-history", IconName::Clock, "命令历史 · Ctrl+R", p)
                        .mb(px(2.))
                        .on_click(
                            cx.listener(|view, _, window, cx| view.history_picker(window, cx)),
                        ),
                )
            });
        col()
            .flex_shrink_0()
            .w_full()
            .min_h(px(49.))
            .bg(p.panel)
            .border_t_1()
            .border_color(p.line)
            .when_some(agent.filter(|_| paste), |el, agent| {
                el.child(
                    row()
                        .gap(px(7.))
                        .px(px(10.))
                        .pt(px(5.))
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .child(icon(IconName::Bot, 13., p.accent))
                        .child(
                            div()
                                .truncate()
                                .child(format!("检测到 {agent}，内容将通过括号粘贴一次发出。")),
                        ),
                )
            })
            .when(broadcast, |el| {
                el.child(
                    div()
                        .px(px(10.))
                        .pt(px(5.))
                        .text_size(px(11.))
                        .text_color(p.warn)
                        .child(format!("⚠ 此命令将发送到 {count} 个已连接会话")),
                )
            })
            .child(
                row()
                    .items_end()
                    .flex_1()
                    .gap(px(if narrow { 4. } else { 7. }))
                    .px(px(9.))
                    .py(px(7.))
                    .child(select)
                    .child(source)
                    .child(
                        row()
                            .h(px(31.))
                            .flex_shrink_0()
                            .rounded(px(6.))
                            .bg(p.hover)
                            .p(px(2.))
                            .children(
                                mode_buttons
                                    .into_iter()
                                    .filter(|(_, mode, _)| !narrow || *mode == paste)
                                    .map(|(id, mode, label)| {
                                        tool(
                                            id,
                                            if narrow {
                                                if mode { "整段 ↻" } else { "逐行 ↻" }
                                            } else {
                                                label
                                            },
                                            label,
                                            p,
                                        )
                                        .when(paste == mode, |el| el.bg(p.panel).text_color(p.ink))
                                        .on_click(
                                            cx.listener(move |view, _, _, cx| {
                                                if let Some(tab) =
                                                    view.active.and_then(|id| view.tab_mut(id))
                                                {
                                                    tab.paste_mode =
                                                        Some(if narrow { !mode } else { mode });
                                                    cx.notify();
                                                }
                                            }),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        primary("send-command", if narrow { "发送" } else { "↵ 发送" }, p)
                            .flex_shrink_0()
                            .disabled(!active_connected || count == 0)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if let Some(id) = active {
                                    view.send_draft(id, window, cx);
                                }
                            })),
                    ),
            )
    }

    fn session_panel(&self, p: Palette, mono: SharedString, cx: &Context<Self>) -> Div {
        let weak = cx.weak_entity();
        let search = self.filter.read(cx).value().to_lowercase();
        let mut groups = Vec::<(String, String, bool, Vec<&ConnectionProfile>)>::new();
        for profile in &self.settings.profiles {
            if !search.split_whitespace().all(|needle| {
                format!(
                    "{} {} {} {}",
                    profile.name, profile.host, profile.user, profile.group
                )
                .to_lowercase()
                .contains(needle)
            }) {
                continue;
            }
            let (key, title, linked) = match &profile.source {
                Some(source) => (
                    format!("source:{source}"),
                    crate::config::display_path(source),
                    true,
                ),
                None if profile.group.trim().is_empty() => {
                    ("group:".to_owned(), "本地会话".to_owned(), false)
                }
                None => (
                    format!("group:{}", profile.group),
                    profile.group.clone(),
                    false,
                ),
            };
            if let Some(group) = groups.iter_mut().find(|(id, _, _, _)| *id == key) {
                group.3.push(profile);
            } else {
                groups.push((key, title, linked, vec![profile]));
            }
        }
        groups.sort_by_key(|group| group.2);
        col()
            .flex_1()
            .min_h_0()
            .bg(p.panel)
            .child(
                pane_head(p)
                    .child(
                        row().gap(px(6.)).child("会话").child(
                            div()
                                .font_family(mono.clone())
                                .font_weight(FontWeight::NORMAL)
                                .text_size(px(11.))
                                .text_color(p.dim)
                                .child(self.settings.profiles.len().to_string()),
                        ),
                    )
                    .child(
                        row()
                            .gap(px(1.))
                            .child(
                                icon_tool(
                                    "new-session",
                                    IconName::Plus,
                                    "新建会话 · Ctrl+Shift+N",
                                    p,
                                )
                                .on_click(cx.listener(
                                    |view, _, window, cx| view.profile_dialog(None, window, cx),
                                )),
                            )
                            .child(
                                icon_tool("import-sessions", IconName::Import, "导入 SSH 配置", p)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.import_dialog(window, cx)
                                    })),
                            )
                            .child(
                                icon_tool("session-options", IconName::Ellipsis, "会话操作", p)
                                    .dropdown_menu_with_anchor(
                                        Anchor::TopRight,
                                        move |menu, _, _| {
                                            menu.item(menu_action(
                                                "命令面板 / 快速连接",
                                                &weak,
                                                |v, w, c| v.command_palette(w, c),
                                            ))
                                            .item(menu_action(
                                                "SSH 与会话设置",
                                                &weak,
                                                |v, w, c| v.settings_dialog(w, c),
                                            ))
                                            .when(
                                                cfg!(windows),
                                                |menu| {
                                                    menu.separator().item(menu_action(
                                                        "打开 PowerShell",
                                                        &weak,
                                                        |v, w, c| {
                                                            v.launch_local(
                                                                LocalShell::PowerShell,
                                                                w,
                                                                c,
                                                            )
                                                        },
                                                    ))
                                                },
                                            )
                                        },
                                    ),
                            ),
                    ),
            )
            .child(
                div().px(px(8.)).pt(px(7.)).pb(px(4.)).child(
                    input(&self.filter, "搜索会话", p)
                        .prefix(icon(IconName::Search, 14., p.dim))
                        .text_size(px(13.)),
                ),
            )
            .child(
                col()
                    .id("saved-session-list")
                    .overflow_y_scroll()
                    .flex_1()
                    .px(px(4.))
                    .pb(px(6.))
                    .when(groups.is_empty(), |el| {
                        el.child(muted_message(
                            if search.is_empty() {
                                "新建会话或导入 SSH 配置，开始管理连接。"
                            } else {
                                "没有匹配的会话。"
                            },
                            p,
                        ))
                    })
                    .children(groups.into_iter().map(|(key, title, linked, profiles)| {
                        let collapsed = search.is_empty() && self.collapsed_groups.contains(&key);
                        let toggle = key.clone();
                        let sync = key
                            .strip_prefix("source:")
                            .and_then(|path| {
                                self.settings
                                    .linked_configs
                                    .iter()
                                    .find(|linked| linked.path == path)
                            })
                            .and_then(|linked| linked.synced_at)
                            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds as i64, 0))
                            .map(|time| {
                                format!(
                                    " · 同步 {}",
                                    time.with_timezone(&chrono::Local).format("%m-%d %H:%M")
                                )
                            })
                            .unwrap_or_default();
                        col()
                            .child(
                                row()
                                    .id(SharedString::from(key))
                                    .h(px(27.))
                                    .gap(px(5.))
                                    .px(px(6.))
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(p.hover))
                                    .child(icon(
                                        if collapsed {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronDown
                                        },
                                        12.,
                                        p.dim,
                                    ))
                                    .when(linked, |el| el.child(icon(IconName::Link, 12., p.dim)))
                                    .child(
                                        div().flex_1().truncate().child(format!("{title}{sync}")),
                                    )
                                    .child(
                                        div()
                                            .text_color(p.dim)
                                            .font_family(mono.clone())
                                            .child(profiles.len().to_string()),
                                    )
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        if !view.collapsed_groups.remove(&toggle) {
                                            view.collapsed_groups.insert(toggle.clone());
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(!collapsed, |el| {
                                el.children(
                                    profiles.into_iter().map(|profile| {
                                        self.profile_row(profile, p, mono.clone(), cx)
                                    }),
                                )
                            })
                    })),
            )
    }

    fn profile_row(
        &self,
        profile: &ConnectionProfile,
        p: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let matches = |tab: &&Tab| {
            tab.ssh_profile()
                .is_some_and(|other| other.id == profile.id)
        };
        let opened = self
            .active_tab()
            .filter(matches)
            .or_else(|| {
                self.tabs
                    .iter()
                    .filter(matches)
                    .find(|tab| tab.phase == Phase::Connected)
            })
            .or_else(|| self.tabs.iter().find(matches));
        let selected = opened.is_some_and(|tab| Some(tab.id) == self.active);
        let phase = opened.map_or(Phase::Disconnected, |tab| tab.phase);
        let hops = profile.jumps.len();
        let view = cx.weak_entity();
        let connect = profile.clone();
        let edit = profile.clone();
        let context = profile.clone();
        let tooltip_profile = format!("{} · {}", target(profile), phase.label(false));
        let mut hop = row().gap(px(4.)).text_size(px(11.)).text_color(p.muted);
        if hops > 0 {
            let count = hops.min(3);
            hop = hop
                .child(
                    row()
                        .children((0..count).flat_map(|_| {
                            [
                                div().size(px(4.)).rounded_full().bg(p.route),
                                div().w(px(5.)).h(px(1.)).bg(p.route),
                            ]
                        }))
                        .child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .border_1()
                                .border_color(p.route),
                        ),
                )
                .child(format!("{hops} 跳"));
        }
        row()
            .id(SharedString::from(format!("saved:{}", profile.id)))
            .group("saved-session")
            .h(px(38.))
            .gap(px(7.))
            .rounded(px(5.))
            .pl(px(9.))
            .pr(px(5.))
            .cursor_pointer()
            .tab_stop(true)
            .bg(if selected { p.tint } else { p.panel })
            .hover(|s| s.bg(if selected { p.tint } else { p.hover }))
            .child(status_dot(phase, p, 6.))
            .child(
                col()
                    .flex_1()
                    .justify_center()
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .line_height(px(17.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(profile.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .line_height(px(15.))
                            .font_family(mono)
                            .text_color(p.muted)
                            .child(target(profile)),
                    ),
            )
            .child(hop)
            .child(
                row()
                    .gap(px(0.))
                    .invisible()
                    .group_hover("saved-session", |s| s.visible())
                    .child(
                        icon_tool("row-connect", IconName::Play, "连接", p)
                            .size(px(22.))
                            .min_w(px(22.))
                            .tooltip(tooltip_profile)
                            .on_click(cx.listener({
                                let connect = connect.clone();
                                move |view, _, window, cx| {
                                    view.connect(connect.clone(), None, window, cx)
                                }
                            })),
                    )
                    .child(
                        icon_tool("row-edit", IconName::Pencil, "编辑会话", p)
                            .size(px(22.))
                            .min_w(px(22.))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.profile_dialog(Some(edit.clone()), window, cx)
                            })),
                    ),
            )
            .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 || event.is_keyboard() {
                    view.connect(connect.clone(), None, window, cx);
                }
            }))
            .context_menu(move |menu, _, _| {
                let new_tab = context.clone();
                let password = context.clone();
                let edit = context.clone();
                let copy = context.clone();
                let test = context.clone();
                let test_password = context.clone();
                let command = context.ssh_command();
                let export = context.clone();
                let move_to = context.clone();
                let remove = context.clone();
                menu.item(menu_action("在新标签连接", &view, move |v, w, c| {
                    v.connect_new(new_tab.clone(), None, w, c)
                }))
                .item(menu_action("用密码连接…", &view, move |v, w, c| {
                    v.password_dialog(password.clone(), w, c)
                }))
                .item(menu_action("编辑会话…", &view, move |v, w, c| {
                    v.profile_dialog(Some(edit.clone()), w, c)
                }))
                .item(menu_action("复制会话", &view, move |v, w, c| {
                    let mut duplicate = copy.clone();
                    duplicate.id.clear();
                    duplicate.source = None;
                    duplicate.name.push_str(" 副本");
                    v.profile_dialog(Some(duplicate), w, c);
                }))
                .item(menu_action("测试连接", &view, move |v, _, c| {
                    v.test(test.clone(), None, c)
                }))
                .item(menu_action(
                    "用密码测试连接…",
                    &view,
                    move |v, w, c| v.test_with_password(test_password.clone(), w, c),
                ))
                .item(menu_action(
                    "复制 SSH 命令",
                    &view,
                    move |view, _, cx| {
                        if command.is_empty() {
                            view.announce("此代理设置无法直接转换为 OpenSSH 命令", cx);
                        } else {
                            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                        }
                    },
                ))
                .when(context.source.is_none(), |menu| {
                    menu.item(menu_action(
                        "导出 SSH 配置片段…",
                        &view,
                        move |v, w, c| {
                            v.export_ssh_config(&export, w, c);
                        },
                    ))
                    .item(menu_action(
                        "移动到分组…",
                        &view,
                        move |v, w, c| {
                            v.move_profile_group(move_to.clone(), w, c);
                        },
                    ))
                })
                .separator()
                .item(menu_action("删除会话…", &view, move |v, w, c| {
                    v.remove_profile(remove.clone(), w, c)
                }))
            })
            .into_any_element()
    }

    fn status(&self, p: Palette, mono: SharedString, width: Pixels, cx: &Context<Self>) -> Div {
        let tab = self.active_tab();
        let phase = tab.map_or(Phase::Disconnected, |tab| tab.phase);
        let local = tab.is_some_and(Tab::is_local);
        let mut status = row()
            .h(px(24.))
            .pl(px(6.))
            .pr(px(4.))
            .flex_shrink_0()
            .bg(p.app)
            .border_t_1()
            .border_color(p.line)
            .text_size(px(12.))
            .text_color(p.muted)
            .overflow_hidden();
        let target = tab
            .and_then(Tab::ssh_profile)
            .map(target)
            .unwrap_or_else(|| {
                if local {
                    "本地".into()
                } else {
                    String::new()
                }
            });
        let cipher = tab
            .and_then(|tab| tab.connection.as_ref()?.cipher())
            .map(str::to_owned);
        let rtt = tab.and_then(|tab| tab.sample.as_ref()?.rtt_ms);
        let transfer_count = self.file_transfer_count();
        status = status.child(if tab.is_some() {
            row()
                .gap(px(5.))
                .px(px(8.))
                .max_w(px(270.))
                .child(status_dot(phase, p, 7.))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(phase_color(phase, p))
                        .child(phase.label(local)),
                )
                .child(div().truncate().font_family(mono.clone()).child(target))
        } else {
            status_field("就绪", None, false, p)
        });
        if width >= px(890.) {
            status = status
                .when_some(cipher, |el, cipher| {
                    el.child(status_field(cipher, Some(IconName::Lock), true, p))
                })
                .when_some(rtt, |el, rtt| {
                    el.child(status_field(
                        format!("往返 {rtt} ms"),
                        Some(IconName::Activity),
                        true,
                        p,
                    ))
                });
        }
        if let Some(tab) = tab
            && phase == Phase::Connecting
            && let Some(profile) = tab.ssh_profile()
        {
            let connected = tab
                .route
                .iter()
                .filter(|hop| hop.stage == HopStage::Connected)
                .count();
            status = status.child(status_field(
                format!("已建立 {connected} / {} 跳", profile.jumps.len() + 1),
                Some(IconName::Waypoints),
                true,
                p,
            ));
        }
        status = status.child(
            div()
                .id("status-notice")
                .flex_1()
                .min_w_0()
                .truncate()
                .text_center()
                .text_color(p.dim)
                .child(self.notice.clone())
                .when(!self.notice.is_empty(), |el| {
                    let detail = self.notice.clone();
                    el.cursor_pointer()
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.notice_dialog(detail.clone(), window, cx)
                        }))
                }),
        );
        if let Some((cursor, syntax, encoding, endings, dirty)) = self.file_editor_status(cx) {
            status = status.children(
                [cursor.as_str(), syntax, encoding, endings]
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| status_field(text.to_owned(), None, index != 0, p)),
            );
            if dirty {
                status = status.child(status_field("未保存", None, true, p).text_color(p.warn));
            }
        } else if let Some(tab) = tab {
            let size = tab.terminal.read(cx).size();
            if width >= px(710.) {
                let kind = tab
                    .ssh_profile()
                    .and_then(|profile| profile.ssh.term.as_deref())
                    .unwrap_or("xterm-256color");
                status = status.child(status_field(kind.to_owned(), None, false, p));
            }
            status = status
                .child(status_field(
                    format!("{}×{}", size.cols, size.rows),
                    None,
                    width >= px(710.),
                    p,
                ))
                .child(status_field("UTF-8", None, true, p));
        }
        status.when(transfer_count > 0, |el| {
            el.child(
                status_field(
                    format!("{transfer_count} 个传输"),
                    Some(IconName::ArrowUpFromLine),
                    tab.is_some(),
                    p,
                )
                .id("status-transfers")
                .cursor_pointer()
                .tab_stop(true)
                .hover(|style| style.bg(p.hover).text_color(p.ink))
                .on_click(cx.listener(|view, _, window, cx| {
                    view.left_visible = true;
                    view.focus_mode = false;
                    view.show_file_transfers(window, cx);
                    if view.compact {
                        view.drawer = Some(Drawer::Files);
                    }
                    cx.notify();
                })),
            )
        })
    }

    fn notification_center(&self, p: Palette, cx: &Context<Self>) -> Div {
        col()
            .absolute()
            .top(px(43.))
            .right(px(9.))
            .w(px(340.))
            .max_h(px(420.))
            .occlude()
            .shadow_lg()
            .bg(p.raised)
            .rounded(px(10.))
            .border_1()
            .border_color(p.line)
            .overflow_hidden()
            .child(pane_head(p).child("通知").child(
                icon_tool("close-notification-center", IconName::X, "关闭通知中心", p).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.notifications_open = false;
                        cx.notify();
                    }),
                ),
            ))
            .child(
                col()
                    .id("terminal-notification-list")
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(self.notifications.is_empty(), |el| {
                        el.child(muted_message("当前没有通知", p))
                    })
                    .children(self.notifications.iter().enumerate().map(|(index, note)| {
                        let id = note.tab;
                        col()
                            .id(("notification", index))
                            .gap(px(4.))
                            .px(px(11.))
                            .py(px(8.))
                            .cursor_pointer()
                            .border_b_1()
                            .border_color(p.line)
                            .hover(|el| el.bg(p.hover))
                            .child(
                                row()
                                    .gap(px(5.))
                                    .child(icon(
                                        IconName::Bell,
                                        13.,
                                        if note.read { p.dim } else { p.accent },
                                    ))
                                    .child(div().flex_1().truncate().child(note.label.clone()))
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(p.dim)
                                            .child(note.time.format("%H:%M").to_string()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(p.muted)
                                    .child(note.message.clone()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_tab(id, window, cx);
                                if let Some(note) = this.notifications.get_mut(index) {
                                    note.read = true;
                                }
                                this.notifications_open = false;
                                cx.notify();
                            }))
                    })),
            )
    }
}

fn status_field(
    text: impl Into<SharedString>,
    symbol: Option<IconName>,
    divided: bool,
    p: Palette,
) -> Div {
    row()
        .h_full()
        .flex_shrink_0()
        .gap(px(5.))
        .px(px(8.))
        .when(divided, |field| {
            field.child(
                div()
                    .w(px(1.))
                    .h(px(12.))
                    .mr(px(3.))
                    .ml(px(-8.))
                    .bg(p.line_strong),
            )
        })
        .when_some(symbol, |field, name| field.child(icon(name, 13., p.dim)))
        .child(text.into())
}

fn route_stage_label(stage: HopStage) -> &'static str {
    match stage {
        HopStage::Connecting => "正在建立连接",
        HopStage::ProxyApproval => "等待确认代理命令",
        HopStage::Trust => "等待确认主机密钥",
        HopStage::Authenticating => "正在认证",
        HopStage::Connected => "连接已建立",
        HopStage::Failed => "连接失败",
    }
}

fn route_segment(id: impl Into<ElementId>, stage: Option<HopStage>, color: Hsla) -> AnyElement {
    if matches!(stage, Some(HopStage::Connected | HopStage::Failed)) {
        return div()
            .h(px(2.))
            .w(px(12.))
            .rounded_full()
            .bg(color)
            .into_any_element();
    }
    let marks = || (0..4).map(|_| div().h(px(1.)).w(px(3.)).flex_shrink_0().bg(color));
    let track = div().relative().w(px(13.)).h(px(3.)).overflow_hidden();
    if stage.is_some() {
        track
            .child(
                row()
                    .absolute()
                    .left(px(-5.))
                    .top(px(1.))
                    .gap(px(2.))
                    .children(marks())
                    .with_animation(
                        id,
                        Animation::new(Duration::from_millis(450))
                            .repeat()
                            .with_max_fps(24.),
                        |row, progress| row.left(px(-5. + 5. * progress)),
                    ),
            )
            .into_any_element()
    } else {
        track
            .child(row().absolute().top(px(1.)).gap(px(2.)).children(marks()))
            .into_any_element()
    }
}

fn phase_color(phase: Phase, p: Palette) -> Hsla {
    match phase {
        Phase::Connected => p.green,
        Phase::Connecting => p.warn,
        Phase::Failed => p.danger,
        Phase::Disconnected => p.dim,
    }
}

fn status_dot(phase: Phase, p: Palette, size: f32) -> Div {
    let color = phase_color(phase, p);
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .when(
            matches!(phase, Phase::Connecting | Phase::Disconnected),
            |el| el.border_1().border_color(color),
        )
        .when(matches!(phase, Phase::Connected | Phase::Failed), |el| {
            el.bg(color)
        })
}

pub(super) fn target(profile: &ConnectionProfile) -> String {
    crate::config::ssh_address(
        &profile.user,
        &profile.host,
        (profile.port != 22).then_some(profile.port),
    )
}
