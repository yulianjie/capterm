use gpui_kit::component::Theme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;

use super::*;
use crate::config::{
    self, Settings, ThemePreference, TimestampPreference, TransferConflictPreference,
};

const NAV: [(IconName, &str); 6] = [
    (IconName::SunMoon, "外观"),
    (IconName::SquareTerminal, "终端"),
    (IconName::KeyRound, "SSH 与会话"),
    (IconName::ArrowUpFromLine, "文件传输"),
    (IconName::Activity, "主机监控"),
    (IconName::Keyboard, "快捷键"),
];
type View = WeakEntity<Workbench>;

fn settings_section(title: &'static str, description: &'static str, palette: Palette) -> Div {
    col()
        .flex_shrink_0()
        .px(px(24.))
        .pt(px(18.))
        .pb(px(4.))
        .border_color(palette.line)
        .child(
            div()
                .mb(px(4.))
                .text_size(px(13.))
                .line_height(px(19.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when(!description.is_empty(), |section| {
            section.child(
                div()
                    .mb(px(14.))
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(palette.dim)
                    .child(description),
            )
        })
}

fn setting_line(
    label: &'static str,
    detail: &'static str,
    control: impl IntoElement,
    palette: Palette,
) -> Div {
    row()
        .flex_shrink_0()
        .gap(px(24.))
        .py(px(12.))
        .border_t_1()
        .border_color(palette.line)
        .child(
            col()
                .flex_1()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(19.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .when(!detail.is_empty(), |line| {
                    line.child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(17.))
                            .text_color(palette.dim)
                            .child(detail),
                    )
                }),
        )
        .child(
            row()
                .w(px(220.))
                .max_w(relative(0.48))
                .flex_shrink_0()
                .justify_end()
                .child(control),
        )
}

pub(super) fn toggle_setting(
    id: &'static str,
    checked: bool,
    view: &View,
    set: fn(&mut Settings, bool),
) -> Switch {
    let view = view.clone();
    Switch::new(id)
        .checked(checked)
        .on_click(move |checked, _, cx| {
            let _ = view.update(cx, |this, cx| {
                let mut settings = this.settings.clone();
                set(&mut settings, *checked);
                if this.persist(settings, cx) {
                    this.apply_terminal_preferences(cx);
                }
            });
        })
}

fn choices(
    id: &'static str,
    value: String,
    options: Vec<(u64, String)>,
    width: f32,
    palette: Palette,
    view: &View,
    set: fn(&mut Settings, u64),
) -> Div {
    let view = view.clone();
    dropdown_field(id, value, options, width, palette, move |id, cx| {
        let _ = view.update(cx, |this, cx| {
            let mut settings = this.settings.clone();
            set(&mut settings, id);
            if this.persist(settings, cx) {
                this.apply_terminal_preferences(cx);
            }
        });
    })
}

fn dropdown_field<T: Clone + 'static>(
    id: &'static str,
    value: String,
    options: Vec<(T, String)>,
    width: f32,
    palette: Palette,
    select: impl Fn(T, &mut App) + 'static,
) -> Div {
    let select = Rc::new(select);
    div().w(px(width)).min_w_0().flex_shrink_1().child(
        Button::new(id)
            .secondary()
            .outline()
            .w_full()
            .h(px(28.))
            .px(px(8.))
            .rounded(px(6.))
            .bg(palette.input)
            .border_color(palette.line)
            .accessibility_label(value.clone())
            .child(
                row()
                    .w_full()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_left()
                            .text_size(px(12.5))
                            .text_color(palette.ink)
                            .child(value.clone()),
                    )
                    .child(div().flex_shrink_0().child(icon(
                        IconName::ChevronsUpDown,
                        12.,
                        palette.muted,
                    ))),
            )
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                options
                    .iter()
                    .fold(menu.min_w(px(width)), |menu, (option, label)| {
                        let option = option.clone();
                        let select = select.clone();
                        menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(*label == value)
                                .on_click(move |_, _, cx| select(option.clone(), cx)),
                        )
                    })
            }),
    )
}

fn font_picker(
    id: &'static str,
    current: &str,
    candidates: &[&'static str],
    installed: &[String],
    palette: Palette,
    view: &View,
    set: fn(&mut Settings, Option<String>),
) -> Div {
    let options = candidates
        .iter()
        .enumerate()
        .filter(|(index, family)| {
            *index == 0
                || installed
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(family))
        })
        .map(|(index, family)| ((index > 0).then_some(*family), (*family).to_owned()))
        .collect();
    let view = view.clone();
    dropdown_field(
        id,
        current.to_owned(),
        options,
        200.,
        palette,
        move |font, cx| {
            let _ = view.update(cx, |this, cx| {
                let mut settings = this.settings.clone();
                set(&mut settings, font.map(str::to_owned));
                if this.persist(settings, cx) {
                    this.apply_font_preferences(cx);
                }
            });
        },
    )
}

fn fixed_value(value: impl Into<SharedString>, width: f32, palette: Palette) -> Div {
    row()
        .w(px(width))
        .h(px(28.))
        .px(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(palette.line)
        .bg(palette.input)
        .child(
            div()
                .truncate()
                .text_size(px(12.5))
                .text_color(palette.ink)
                .child(value.into()),
        )
}

fn settings_head(path: &str, palette: Palette) -> Div {
    row()
        .items_start()
        .gap(px(12.))
        .pl(px(20.))
        .pr(px(12.))
        .pt(px(14.))
        .pb(px(12.))
        .border_b_1()
        .border_color(palette.line)
        .flex_shrink_0()
        .child(
            col()
                .flex_1()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(22.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("设置"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(palette.dim)
                        .truncate()
                        .child(format!("修改即时生效，保存在 {path}")),
                ),
        )
        .child(
            chrome_icon_tool("close-settings", IconName::X, "关闭", palette)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
}

fn miniature(theme: ThemePreference, palette: Palette) -> Div {
    div()
        .h(px(104.))
        .w_full()
        .flex_shrink_0()
        .overflow_hidden()
        .rounded(px(6.))
        .border_1()
        .border_color(palette.line)
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| paint_miniature(bounds, theme, window),
            )
            .size_full(),
        )
}

fn paint_miniature(bounds: Bounds<Pixels>, theme: ThemePreference, window: &mut Window) {
    let dark = Palette::new(false);
    let light = Palette::new(true);
    let width = bounds.size.width;
    let height = bounds.size.height;
    let left = width * 0.26;
    let right = width * 0.76;
    let top_bar = px(10.);
    let bottom_bar = height - px(6.);
    let mut piece = |x, y, width, height, dark_color, light_color| {
        let rect = Bounds::new(bounds.origin + point(x, y), size(width, height));
        if theme == ThemePreference::System {
            paint_miniature_half(window, rect, bounds, dark_color, false);
            paint_miniature_half(window, rect, bounds, light_color, true);
        } else {
            let color = if theme == ThemePreference::Light {
                light_color
            } else {
                dark_color
            };
            window.paint_quad(fill(rect, color));
        }
    };
    piece(px(0.), px(0.), width, height, dark.app, light.app);
    piece(
        px(0.),
        top_bar,
        width,
        bottom_bar - top_bar,
        dark.panel,
        light.panel,
    );
    piece(
        left,
        top_bar,
        right - left,
        bottom_bar - top_bar,
        dark.base,
        light.base,
    );
    for x in [left, right] {
        piece(
            x,
            top_bar,
            px(1.),
            bottom_bar - top_bar,
            dark.line,
            light.line,
        );
    }
    for y in [top_bar, bottom_bar] {
        piece(px(0.), y, width, px(1.), dark.line, light.line);
    }

    let columns: [(Pixels, Pixels, &[f32]); 3] = [
        (px(4.), left - px(8.), &[0.8, 0.6, 0.7, 0.55]),
        (
            left + px(6.),
            right - left - px(12.),
            &[0.7, 0.46, 0.84, 0.62, 0.74, 0.38, 0.58],
        ),
        (right + px(4.), width - right - px(8.), &[0.7, 0.8, 0.5]),
    ];
    for (column, (x, span, bars)) in columns.into_iter().enumerate() {
        for (index, &proportion) in bars.iter().enumerate() {
            let (dark_color, light_color) = match (column, index) {
                (1, 0) => (dark.route, light.route),
                (1, 1 | 5) => (dark.green, light.green),
                (1, _) => (dark.ink.opacity(0.45), light.ink.opacity(0.45)),
                (0, 1) | (2, 2) => (dark.accent, light.accent),
                _ => (dark.line_strong, light.line_strong),
            };
            piece(
                x,
                top_bar + px(5. + index as f32 * 6.),
                span.max(px(0.)) * proportion,
                if (column, index) == (1, 0) {
                    px(2.)
                } else {
                    px(3.)
                },
                dark_color,
                light_color,
            );
        }
    }
}

fn paint_miniature_half(
    window: &mut Window,
    rect: Bounds<Pixels>,
    preview: Bounds<Pixels>,
    color: Hsla,
    light: bool,
) {
    // The same diagonal crosses the entire preview, including each painted line.
    let distance = |point: Point<Pixels>| {
        let fraction = f32::from(point.y - preview.top()) / f32::from(preview.size.height);
        let split = preview.left() + preview.size.width * (0.62 - 0.24 * fraction);
        f32::from(point.x - split) * if light { 1. } else { -1. }
    };
    let corners = [
        rect.origin,
        rect.top_right(),
        rect.bottom_right(),
        rect.bottom_left(),
    ];
    let distances = corners.map(distance);
    if distances.iter().all(|distance| *distance >= 0.) {
        window.paint_quad(fill(rect, color));
        return;
    }
    if distances.iter().all(|distance| *distance <= 0.) {
        return;
    }
    let mut polygon = Vec::with_capacity(5);
    for index in 0..corners.len() {
        let next = (index + 1) % corners.len();
        let start = corners[index];
        let end = corners[next];
        let start_distance = distances[index];
        let end_distance = distances[next];
        if start_distance >= 0. {
            polygon.push(start);
        }
        if (start_distance >= 0.) != (end_distance >= 0.) {
            let ratio = start_distance / (start_distance - end_distance);
            polygon.push(start + (end - start) * ratio);
        }
    }
    if let [start, rest @ ..] = polygon.as_slice() {
        let mut path = PathBuilder::fill();
        path.move_to(*start);
        for point in rest {
            path.line_to(*point);
        }
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}
fn theme_cards(settings: &Settings, view: &View, palette: Palette, show_caption: bool) -> Div {
    let selected = match (settings.theme, settings.light) {
        (ThemePreference::Legacy, true) => ThemePreference::Light,
        (ThemePreference::Legacy, false) => ThemePreference::Dark,
        (theme, _) => theme,
    };
    row().items_stretch().gap(px(12.)).children(
        [
            (ThemePreference::Dark, "深色", "夜航"),
            (ThemePreference::Light, "浅色", "白昼"),
            (
                ThemePreference::System,
                "跟随系统",
                if settings.light {
                    "当前为浅色"
                } else {
                    "当前为深色"
                },
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(id, (choice, title, subtitle))| {
            let active = selected == choice;
            let view = view.clone();
            Button::new(("theme-card", id))
                .ghost()
                .accessibility_label(title)
                .flex_1()
                .flex_shrink_1()
                .min_w_0()
                .h_auto()
                .p(px(if active { 7. } else { 8. }))
                .when(active, |card| card.border_2().border_color(palette.accent))
                .when(!active, |card| card.border_1().border_color(palette.line))
                .bg(palette.input)
                .rounded(px(10.))
                .cursor_pointer()
                .child(
                    col()
                        .w_full()
                        .gap(px(8.))
                        .child(miniature(choice, palette))
                        .child(
                            row()
                                .gap(px(8.))
                                .px(px(2.))
                                .pb(px(2.))
                                .text_size(px(12.5))
                                .line_height(px(18.))
                                .text_color(palette.ink)
                                .child(
                                    div()
                                        .size(px(15.))
                                        .flex_shrink_0()
                                        .rounded_full()
                                        .bg(palette.input)
                                        .when(active, |radio| {
                                            radio.border_4().border_color(palette.accent)
                                        })
                                        .when(!active, |radio| {
                                            radio.border_2().border_color(palette.line_strong)
                                        }),
                                )
                                .child(div().truncate().child(title))
                                .when(show_caption, |caption| {
                                    caption.child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .text_right()
                                            .text_size(px(11.))
                                            .text_color(palette.dim)
                                            .child(subtitle),
                                    )
                                }),
                        ),
                )
                .on_click(move |_, window, cx| {
                    let _ =
                        view.update(cx, |this, cx| this.set_theme_preference(choice, window, cx));
                })
        }),
    )
}
fn appearance(
    settings: &Settings,
    view: &View,
    palette: Palette,
    installed: &[String],
    show_caption: bool,
    cx: &App,
) -> Div {
    let theme = Theme::global(cx);
    let time_options = [
        "时:分:秒（10:42:07）",
        "时:分:秒.毫秒（10:42:07.125）",
        "相对时间（+42 秒）",
    ];
    let time_index = match settings.timestamp_format {
        TimestampPreference::Seconds => 0,
        TimestampPreference::Milliseconds => 1,
        TimestampPreference::Relative => 2,
    };
    let font_sizes = [10, 11, 12, 13, 14, 15, 16, 18, 20]
        .into_iter()
        .map(|size| (size, size.to_string()))
        .collect();
    let spacing = [12, 14, 15, 17, 20]
        .into_iter()
        .map(|multiple| (multiple, format!("{:.1} 倍", multiple as f32 / 10.)))
        .collect();
    col()
        .flex_shrink_0()
        .child(settings_section("主题", "深色、浅色两套主题等价设计；终端配色、图表和代码高亮随主题一起切换。快捷键 Ctrl+Shift+L。", palette)
            .child(theme_cards(settings, view, palette, show_caption)))
        .child(settings_section("字体", "", palette).border_t_1()
            .child(setting_line("界面字体", "标签、菜单与表单。",
                font_picker("set-ui-font", &theme.font_family,
                    &["Noto Sans SC", "Microsoft YaHei UI", "Segoe UI", "PingFang SC"],
                    installed, palette, view, |s, v| s.ui_font_family = v), palette))
            .child(setting_line("终端与编辑器字体", "建议使用中英文 2:1 等宽的字体，TUI 边框与表格才不会错位。",
                font_picker("set-mono-font", &theme.mono_font_family,
                    &["Maple Mono NF CN", "Sarasa Mono SC", "Cascadia Mono", "Consolas"],
                    installed, palette, view, |s, v| s.mono_font_family = v), palette))
            .child(setting_line("终端字号与行高", "", row().gap(px(8.))
                .child(choices("set-terminal-size", format!("{:.0}", settings.terminal_font_size), font_sizes, 80., palette, view, |s, v| {
                    let ratio = s.terminal_line_height / s.terminal_font_size.max(1.);
                    s.terminal_font_size = v as f32;
                    s.terminal_line_height = (s.terminal_font_size * ratio).round();
                }))
                .child(choices("set-terminal-spacing", format!("{:.1} 倍", settings.terminal_line_height / settings.terminal_font_size.max(1.)), spacing, 100., palette, view, |s, v| {
                    s.terminal_line_height = (s.terminal_font_size * v as f32 / 10.).round();
                })), palette)))
        .child(settings_section("行号与时间", "", palette).border_t_1().pb(px(16.))
            .child(setting_line("显示行号", "按会话累计编号，全屏程序期间显示屏幕行号。",
                toggle_setting("set-line-numbers", settings.show_line_numbers, view, |s, v| s.show_line_numbers = v), palette))
            .child(setting_line("显示时间戳", "每一行记录写入终端的时间；重绘行在滚入历史后定格。",
                toggle_setting("set-timestamps", settings.show_timestamps, view, |s, v| s.show_timestamps = v), palette))
            .child(setting_line("时间格式", "", choices("set-timestamp-format", time_options[time_index].into(),
                time_options.into_iter().enumerate().map(|(index, text)| (index as u64, text.into())).collect(),
                200., palette, view, |s, v| s.timestamp_format = match v {
                    1 => TimestampPreference::Milliseconds,
                    2 => TimestampPreference::Relative,
                    _ => TimestampPreference::Seconds,
                }), palette))
            .child(setting_line("弱化同一秒内重复的时间", "让命令的开始时间更容易被扫到。",
                toggle_setting("set-dim-timestamps", settings.dim_repeated_timestamps, view, |s, v| s.dim_repeated_timestamps = v), palette))
            .child(setting_line("空闲间隔线", "两行之间超出时长时插入分隔线。",
                choices("set-idle-threshold", format!("{} 秒", settings.idle_threshold),
                    [15, 30, 60, 120, 300].into_iter().map(|value| (value, format!("{value} 秒"))).collect(),
                    120., palette, view, |s, v| s.idle_threshold = v), palette)))
}

fn terminal_settings(settings: &Settings, view: &View, palette: Palette) -> Div {
    let actions_view = view.clone();
    let rule_path = config::linux_highlights_path()
        .map(|path| config::display_path(&path.to_string_lossy()))
        .unwrap_or_else(|_| "linux-highlights.json".into());
    settings_section(
        "Linux 文本高亮",
        "在 SSH 与 WSL 终端中强调 Linux 命令输出中的关键文本，可编辑规则后重载。",
        palette,
    )
    .child(setting_line(
        "启用 Linux 文本高亮",
        "应用于 SSH 与 WSL 会话。",
        toggle_setting(
            "set-linux-highlighting",
            settings.linux_highlighting,
            view,
            |s, value| s.linux_highlighting = value,
        ),
        palette,
    ))
    .child(setting_line(
        "高亮规则",
        "编辑后使用“重载规则”立即应用。",
        row()
            .gap(px(8.))
            .child(
                Button::new("edit-linux-highlights")
                    .secondary()
                    .outline()
                    .h(px(28.))
                    .px(px(9.))
                    .child("编辑规则")
                    .on_click(move |_, _, cx| {
                        let _ = actions_view.update(cx, |this, cx| {
                            this.edit_linux_highlights(cx);
                        });
                    }),
            )
            .child({
                let reload_view = view.clone();
                Button::new("reload-linux-highlights")
                    .secondary()
                    .outline()
                    .h(px(28.))
                    .px(px(9.))
                    .child("重载规则")
                    .on_click(move |_, _, cx| {
                        let _ = reload_view.update(cx, |this, cx| {
                            this.reload_linux_highlights(cx);
                        });
                    })
            }),
        palette,
    ))
    .child(setting_line(
        "规则文件位置",
        "编辑规则使用此文件。",
        fixed_value(rule_path, 220., palette),
        palette,
    ))
    .border_b_1()
    .child(
        settings_section(
            "终端兼容",
            "适用于 vim、htop、tmux、Claude Code、Codex、Gemini CLI 等程序。",
            palette,
        )
        .children(
            [
                ("终端类型 TERM", "支持 24 位真彩色。", "xterm-256color"),
                (
                    "Shift+Enter",
                    "启用 Kitty 键盘协议的程序会收到对应编码。",
                    "发送 ESC + Enter",
                ),
                ("Kitty 键盘协议", "程序可请求 CSI u 键盘编码。", "自动协商"),
                ("同步输出（DEC 2026）", "程序可请求整帧绘制。", "由程序控制"),
                (
                    "桌面通知（OSC 9 / 777、响铃）",
                    "程序请求提醒时在工作区标记。",
                    "通知中心",
                ),
            ]
            .into_iter()
            .map(|(label, detail, value)| {
                setting_line(label, detail, fixed_value(value, 200., palette), palette)
            }),
        ),
    )
}

fn ssh_settings(settings: &Settings, view: &View, palette: Palette, mono: SharedString) -> Div {
    let seconds = |values: &[u64]| {
        values
            .iter()
            .map(|&value| {
                (
                    value,
                    if value == 0 {
                        "关闭".into()
                    } else {
                        format!("{value} 秒")
                    },
                )
            })
            .collect()
    };
    let paths = settings
        .linked_configs
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let sync = view.clone();
            let unlink = view.clone();
            let path = source.path.clone();
            let unlink_path = path.clone();
            let count = settings
                .profiles
                .iter()
                .filter(|p| p.source.as_ref() == Some(&path))
                .count();
            let updated = source
                .synced_at
                .and_then(|time| chrono::DateTime::from_timestamp(time as i64, 0))
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "等待同步".into());
            row()
                .gap(px(10.))
                .pl(px(12.))
                .pr(px(8.))
                .py(px(9.))
                .when(index + 1 != settings.linked_configs.len(), |row| {
                    row.border_b_1()
                })
                .border_color(palette.line)
                .child(icon(IconName::FileText, 14., palette.dim))
                .child(
                    col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .child(config::display_path(&path)),
                        )
                        .child(
                            div()
                                .text_color(palette.dim)
                                .text_size(px(11.))
                                .line_height(px(16.))
                                .child(format!("已链接，{count} 个主机 · {updated}")),
                        ),
                )
                .child(
                    tool(("sync-linked", index), "↻", "立即同步", palette).on_click(
                        move |_, window, cx| {
                            let _ = sync.update(cx, |this, cx| {
                                this.refresh_linked_config(path.clone(), window, cx)
                            });
                        },
                    ),
                )
                .child(
                    tool(("unlink-config", index), "×", "移除链接", palette).on_click(
                        move |_, _, cx| {
                            let _ = unlink.update(cx, |this, cx| {
                                let mut settings = this.settings.clone();
                                settings.unlink(&unlink_path);
                                this.persist(settings, cx);
                            });
                        },
                    ),
                )
        });
    let import = |linked: bool| {
        let view = view.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |this, cx| {
                this.open_import(this.settings.config_path.clone(), linked, window, cx)
            });
        }
    };
    let known_hosts = config::known_hosts()
        .map(|p| config::display_path(&p.to_string_lossy()))
        .unwrap_or_default();
    col()
        .flex_shrink_0()
        .child(
            settings_section(
                "SSH 配置文件",
                "按照添加顺序显示各配置文件与已同步会话。",
                palette,
            )
            .when(!settings.linked_configs.is_empty(), |section| {
                section.child(
                    col()
                        .overflow_hidden()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(palette.line)
                        .bg(palette.input)
                        .children(paths),
                )
            })
            .child(
                row()
                    .gap(px(8.))
                    .mt(px(10.))
                    .mb(px(14.))
                    .child(
                        button("settings-link-config", "＋ 添加配置文件…", palette)
                            .on_click(import(true)),
                    )
                    .child(
                        button("settings-copy-config", "从文件导入一次…", palette)
                            .on_click(import(false)),
                    ),
            )
            .child(setting_line(
                "监听文件变化",
                "文件保存后自动重新解析并更新链接会话。",
                toggle_setting(
                    "set-watch-config",
                    settings.watch_ssh_configs,
                    view,
                    |s, v| s.watch_ssh_configs = v,
                ),
                palette,
            )),
        )
        .child(
            settings_section("认证与安全", "", palette)
                .border_t_1()
                .pb(px(16.))
                .child(setting_line(
                    "known_hosts 文件",
                    "",
                    div()
                        .truncate()
                        .font_family(mono)
                        .text_size(px(12.))
                        .text_color(palette.dim)
                        .child(known_hosts),
                    palette,
                ))
                .child(setting_line(
                    "首次连接新主机",
                    "",
                    fixed_value("显示指纹并询问", 180., palette),
                    palette,
                ))
                .child(setting_line(
                    "SSH Agent",
                    "",
                    fixed_value(
                        if cfg!(windows) {
                            "Windows OpenSSH Agent"
                        } else {
                            "SSH_AUTH_SOCK"
                        },
                        200.,
                        palette,
                    ),
                    palette,
                ))
                .child(setting_line(
                    "连接超时",
                    "每一跳单独计时。",
                    choices(
                        "set-ssh-timeout",
                        format!("{} 秒", settings.ssh_timeout),
                        seconds(&[5, 10, 15, 30, 60]),
                        120.,
                        palette,
                        view,
                        |s, value| s.ssh_timeout = value as u32,
                    ),
                    palette,
                ))
                .child(setting_line(
                    "保活间隔",
                    "使用 SSH 协议保活消息。",
                    choices(
                        "set-ssh-keepalive",
                        if settings.ssh_keepalive == 0 {
                            "关闭".into()
                        } else {
                            format!("{} 秒", settings.ssh_keepalive)
                        },
                        seconds(&[0, 15, 30, 60, 120]),
                        120.,
                        palette,
                        view,
                        |s, value| s.ssh_keepalive = value as u32,
                    ),
                    palette,
                ))
                .child(setting_line(
                    "复用跳板连接",
                    "多个会话可以共用已验证且已认证的跳板连接。",
                    toggle_setting("set-reuse-jumps", settings.reuse_jumps, view, |s, value| {
                        s.reuse_jumps = value
                    }),
                    palette,
                )),
        )
}

fn transfer_settings(
    settings: &Settings,
    view: &View,
    palette: Palette,
    mono: SharedString,
) -> Div {
    let downloads = if settings.download_directory.is_empty() {
        dirs::download_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "下载".into())
    } else {
        settings.download_directory.clone()
    };
    let choice = view.clone();
    let conflicts = [
        (TransferConflictPreference::Ask, "在队列中询问"),
        (TransferConflictPreference::Overwrite, "覆盖"),
        (TransferConflictPreference::Skip, "跳过"),
        (TransferConflictPreference::KeepBoth, "保留两者"),
    ];
    let conflict = conflicts
        .iter()
        .find(|(preference, _)| *preference == settings.transfer_conflict)
        .map_or("在队列中询问", |(_, label)| label);
    settings_section("文件传输", "", palette)
        .pb(px(16.))
        .child(setting_line(
            "默认下载位置",
            "",
            row()
                .w_full()
                .gap(px(8.))
                .child(
                    div()
                        .truncate()
                        .flex_1()
                        .font_family(mono)
                        .text_size(px(12.))
                        .text_color(palette.dim)
                        .child(config::display_path(&downloads)),
                )
                .child(
                    button("change-download-location", "更改…", palette)
                        .h(px(22.))
                        .text_size(px(11.5))
                        .on_click(move |_, window, cx| {
                            let picker = cx.prompt_for_paths(PathPromptOptions {
                                files: false,
                                directories: true,
                                multiple: false,
                                prompt: Some("选择下载目录".into()),
                            });
                            let view = choice.clone();
                            window
                                .spawn(cx, async move |cx| {
                                    if let Ok(Ok(Some(paths))) = picker.await
                                        && let Some(path) = paths.first()
                                    {
                                        let _ = cx.update(|_, cx| {
                                            let _ = view.update(cx, |this, cx| {
                                                let mut settings = this.settings.clone();
                                                settings.download_directory =
                                                    path.to_string_lossy().into_owned();
                                                this.persist(settings, cx);
                                            });
                                        });
                                    }
                                })
                                .detach();
                        }),
                ),
            palette,
        ))
        .child(setting_line(
            "同时传输",
            "",
            choices(
                "set-transfer-concurrency",
                format!("{} 个文件", settings.transfer_concurrency),
                [1, 2, 3, 4, 5, 8]
                    .into_iter()
                    .map(|value| (value, format!("{value} 个文件")))
                    .collect(),
                120.,
                palette,
                view,
                |s, value| s.transfer_concurrency = value as usize,
            ),
            palette,
        ))
        .child(setting_line(
            "目标已存在时",
            "",
            choices(
                "set-transfer-conflict",
                conflict.into(),
                conflicts
                    .into_iter()
                    .enumerate()
                    .map(|(index, (_, label))| (index as u64, label.into()))
                    .collect(),
                160.,
                palette,
                view,
                |s, value| {
                    s.transfer_conflict = match value {
                        1 => TransferConflictPreference::Overwrite,
                        2 => TransferConflictPreference::Skip,
                        3 => TransferConflictPreference::KeepBoth,
                        _ => TransferConflictPreference::Ask,
                    }
                },
            ),
            palette,
        ))
        .child(setting_line(
            "保留修改时间与权限",
            "",
            toggle_setting(
                "set-transfer-metadata",
                settings.transfer_preserve_metadata,
                view,
                |s, value| s.transfer_preserve_metadata = value,
            ),
            palette,
        ))
        .child(setting_line(
            "点击打开文本文件的大小上限",
            "更大的文本文件以只读方式预览，或下载后在本机打开。",
            choices(
                "set-edit-limit",
                format!("{} MB", settings.file_edit_limit_mb),
                [1, 2, 5, 10, 20, 50]
                    .into_iter()
                    .map(|value| (value, format!("{value} MB")))
                    .collect(),
                120.,
                palette,
                view,
                |s, value| s.file_edit_limit_mb = value,
            ),
            palette,
        ))
        .child(setting_line(
            "保存方式",
            "远程文件先写入同目录的临时文件。",
            fixed_value("原子替换", 140., palette),
            palette,
        ))
}

fn monitor_settings(settings: &Settings, view: &View, palette: Palette) -> Div {
    let intervals = [2, 5, 10, 15, 30]
        .into_iter()
        .map(|v| (v, format!("{v} 秒")))
        .collect();
    let thresholds = [50, 60, 70, 75, 80, 85, 90, 95, 98, 100];
    let warning = thresholds
        .into_iter()
        .filter(|&v| v < u64::from(settings.monitor_danger_pct))
        .map(|v| (v, format!("{v}%")))
        .collect();
    let danger = thresholds
        .into_iter()
        .filter(|&v| v > u64::from(settings.monitor_warn_pct))
        .map(|v| (v, format!("{v}%")))
        .collect();
    settings_section(
        "主机监控",
        "复用当前 SSH 连接执行只读命令，采集 CPU、内存、磁盘和网络数据。",
        palette,
    )
    .child(setting_line(
        "采样间隔",
        "",
        choices(
            "set-monitor-interval",
            format!("{} 秒", settings.monitor_interval),
            intervals,
            120.,
            palette,
            view,
            |s, v| s.monitor_interval = v,
        ),
        palette,
    ))
    .child(setting_line(
        "资源偏高 / 告急",
        "阈值应用于 CPU、内存和磁盘。",
        row()
            .gap(px(8.))
            .child(choices(
                "set-monitor-warn",
                format!("{}%", settings.monitor_warn_pct),
                warning,
                90.,
                palette,
                view,
                |s, v| s.monitor_warn_pct = v as u8,
            ))
            .child(choices(
                "set-monitor-danger",
                format!("{}%", settings.monitor_danger_pct),
                danger,
                90.,
                palette,
                view,
                |s, v| s.monitor_danger_pct = v as u8,
            )),
        palette,
    ))
}

fn shortcuts(palette: Palette) -> Div {
    const KEYS: [(&str, &str); 16] = [
        ("命令面板 / 快速连接", "Ctrl+Shift+P"),
        ("新建会话", "Ctrl+Shift+N"),
        ("复制当前会话到新标签", "Ctrl+Shift+T"),
        ("关闭标签", "Ctrl+Shift+W"),
        ("切换标签", "Ctrl+Tab / Ctrl+Shift+Tab"),
        ("终端内搜索", "Ctrl+Shift+F"),
        ("复制 / 粘贴", "Ctrl+Shift+C / Ctrl+Shift+V"),
        ("聚焦输入栏", "Ctrl+Shift+I"),
        ("行号与时间", "Ctrl+Shift+G"),
        ("文件面板", "Ctrl+Shift+E"),
        ("会话与监控面板", "Ctrl+Shift+B"),
        ("切换主题", "Ctrl+Shift+L"),
        ("专注模式", "Ctrl+Shift+Enter"),
        ("保存文件", "Ctrl+S"),
        ("设置", "Ctrl+,"),
        ("关闭对话框", "Esc"),
    ];
    settings_section(
        "快捷键",
        "全局快捷键使用 Ctrl+Shift，避免占用终端程序的 Ctrl 组合键。",
        palette,
    )
    .children(KEYS.into_iter().map(|(label, keys)| {
        setting_line(
            label,
            "",
            row().justify_end().gap(px(5.)).flex_wrap().children(
                keys.split(" / ").enumerate().map(|(index, chord)| {
                    row()
                        .gap(px(3.))
                        .when(index > 0, |chord| {
                            chord.child(div().text_color(palette.dim).child("或"))
                        })
                        .children(chord.split('+').map(|key| {
                            div()
                                .border_1()
                                .border_color(palette.line)
                                .rounded(px(4.))
                                .px(px(5.))
                                .text_size(px(11.))
                                .text_color(palette.muted)
                                .child(key)
                        }))
                }),
            ),
            palette,
        )
        .py(px(8.))
    }))
}

impl Workbench {
    pub(in crate::workbench) fn settings_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = Rc::new(Cell::new(0));
        let view = cx.weak_entity();
        let installed = cx.text_system().all_font_names();
        let path = if cfg!(windows) {
            r"%APPDATA%\CapTerm\settings.json".to_owned()
        } else {
            config::settings_path()
                .map(|path| config::display_path(&path.to_string_lossy()))
                .unwrap_or_else(|_| "settings.json".into())
        };
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(entity) = view.upgrade() else {
                return dialog;
            };
            let settings = &entity.read(cx).settings;
            let palette = Palette::new(settings.light);
            let viewport = window.viewport_size();
            let width = px(980.).min(viewport.width - px(40.));
            let nav_width = if width < px(630.) { 128. } else { 200. };
            let nav = navigation(nav_width, palette)
                .px(px(8.))
                .gap(px(2.))
                .bg(palette.panel.opacity(0.4))
                .children(NAV.into_iter().enumerate().map(|(index, (symbol, title))| {
                    let selected = selected.clone();
                    let active = index == selected.get();
                    Button::new(("settings-nav", index))
                        .ghost()
                        .accessibility_label(title)
                        .w_full()
                        .h(px(32.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .when(active, |nav| nav.bg(palette.tint))
                        .child(
                            row()
                                .w_full()
                                .gap(px(9.))
                                .child(icon(
                                    symbol,
                                    16.,
                                    if active { palette.accent } else { palette.dim },
                                ))
                                .child(
                                    div()
                                        .truncate()
                                        .text_left()
                                        .text_size(px(12.5))
                                        .text_color(if active {
                                            palette.ink
                                        } else {
                                            palette.muted
                                        })
                                        .child(title),
                                ),
                        )
                        .on_click(move |_, window, _| {
                            selected.set(index);
                            window.refresh();
                        })
                }));
            let theme = Theme::global(cx);
            let body = match selected.get() {
                0 => appearance(settings, &view, palette, &installed, width >= px(720.), cx),
                1 => terminal_settings(settings, &view, palette),
                2 => ssh_settings(settings, &view, palette, theme.mono_font_family.clone()),
                3 => transfer_settings(settings, &view, palette, theme.mono_font_family.clone()),
                4 => monitor_settings(settings, &view, palette),
                _ => shortcuts(palette),
            };
            styled_dialog(dialog, f32::from(width), palette).child(
                col()
                    .h(px(700.).min(viewport.height - px(40.)))
                    .font_family(theme.font_family.clone())
                    .child(settings_head(&path, palette))
                    .child(
                        row().items_stretch().flex_1().min_h_0().child(nav).child(
                            col()
                                .id(("settings-body", selected.get()))
                                .flex_1()
                                .overflow_y_scroll()
                                .child(body),
                        ),
                    ),
            )
        });
    }
}
