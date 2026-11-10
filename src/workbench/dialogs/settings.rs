mod idle_input;
mod shortcut_editor;

use gpui_kit::component::Theme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{IndexPath, Sizable};

use self::idle_input::IdleInput;
use super::*;
use crate::config::{
    self, Osc52WritePreference, Settings, ShiftEnterPreference, TerminalNotificationPreference,
    ThemePreference, TimestampPreference, TransferConflictPreference,
};
use crate::workbench::ui::font_family_index;

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

fn number_options(
    values: impl IntoIterator<Item = u64>,
    label: impl Fn(u64) -> String,
) -> Vec<(u64, String)> {
    values
        .into_iter()
        .map(|value| (value, label(value)))
        .collect()
}

fn scrollback_label(lines: u64) -> String {
    if lines < 1_000 {
        format!("{lines} 行")
    } else {
        format!("{} {:03} 行", lines / 1_000, lines % 1_000)
    }
}

fn seconds_label(seconds: u64) -> String {
    format!("{seconds} 秒")
}

fn keepalive_label(seconds: u64) -> String {
    if seconds == 0 {
        "关闭".into()
    } else {
        seconds_label(seconds)
    }
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

fn preference_field<T: Copy + PartialEq + 'static>(
    id: &'static str,
    current: T,
    options: &[(T, &str)],
    width: f32,
    palette: Palette,
    select: impl Fn(T, &mut App) + 'static,
) -> Div {
    let label = options
        .iter()
        .find(|(value, _)| *value == current)
        .map_or("", |(_, label)| *label);
    dropdown_field(
        id,
        label.to_owned(),
        options
            .iter()
            .map(|&(value, label)| (value, label.to_owned()))
            .collect(),
        width,
        palette,
        select,
    )
}

fn ssh_value_field(
    id: &'static str,
    current: Option<&str>,
    presets: &[(Option<&str>, &str)],
    view: &View,
    palette: Palette,
    set: fn(&mut Settings, Option<String>),
) -> Div {
    let mut options: Vec<_> = presets
        .iter()
        .map(|&(value, label)| (value.map(str::to_owned), label.to_owned()))
        .collect();
    let label = if let Some((_, label)) = options
        .iter()
        .find(|(value, _)| value.as_deref() == current)
    {
        label.clone()
    } else {
        let value = current.unwrap_or_default().to_owned();
        options.insert(0, (Some(value.clone()), value.clone()));
        value
    };
    let view = view.clone();
    dropdown_field(id, label, options, 200., palette, move |value, cx| {
        let _ = view.update(cx, |this, cx| {
            let mut settings = this.settings.clone();
            set(&mut settings, value);
            this.persist(settings, cx);
        });
    })
}

#[derive(Clone, Copy)]
enum FontRole {
    Ui,
    Mono,
}

impl FontRole {
    fn effective(self, theme: &Theme) -> &SharedString {
        match self {
            Self::Ui => &theme.font_family,
            Self::Mono => &theme.mono_font_family,
        }
    }

    fn theme_family(self, theme: &Theme) -> Option<&SharedString> {
        let config = if theme.is_dark() {
            &theme.dark_theme
        } else {
            &theme.light_theme
        };
        match self {
            Self::Ui => config.font_family.as_ref(),
            Self::Mono => config.mono_font_family.as_ref(),
        }
    }

    fn setting(self, settings: &mut Settings) -> &mut Option<String> {
        match self {
            Self::Ui => &mut settings.ui_font_family,
            Self::Mono => &mut settings.mono_font_family,
        }
    }
}

struct FontPicker {
    state: Entity<SelectState<SearchableVec<SharedString>>>,
    _subscription: Subscription,
}

impl FontPicker {
    fn new(
        role: FontRole,
        installed: &[String],
        window: &mut Window,
        cx: &mut Context<Workbench>,
    ) -> Self {
        let current = role.effective(Theme::global(cx));
        let mut options: Vec<_> = installed.iter().cloned().map(SharedString::from).collect();
        let selected = font_family_index(&options, current).unwrap_or_else(|| {
            options.push(current.clone());
            options.len() - 1
        });
        let families = options.clone();
        let state = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(options),
                Some(IndexPath::new(selected)),
                window,
                cx,
            )
            .searchable(true)
        });
        let subscription =
            cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                let SelectEvent::Confirm(Some(choice)) = event else {
                    return;
                };
                let theme = Theme::global(cx);
                let family = role
                    .theme_family(theme)
                    .is_none_or(|default| !default.eq_ignore_ascii_case(choice))
                    .then(|| choice.to_string());
                let mut settings = this.settings.clone();
                let setting = role.setting(&mut settings);
                if *setting == family {
                    return;
                }
                *setting = family;
                if this.persist(settings, cx) {
                    this.apply_font_preferences(cx);
                } else {
                    let current = role.effective(Theme::global(cx));
                    let selected = font_family_index(&families, current).map(IndexPath::new);
                    state.update(cx, |state, cx| {
                        state.set_selected_index(selected, window, cx);
                        cx.notify();
                    });
                }
            });
        Self {
            state,
            _subscription: subscription,
        }
    }

    fn render(&self, id: &'static str, label: &'static str, palette: Palette) -> Div {
        div()
            .w(px(200.))
            .h(px(28.))
            .min_w_0()
            .flex_shrink_1()
            .child(
                Select::new(&self.state)
                    .id(id)
                    .small()
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .bg(palette.input)
                    .border_color(palette.line)
                    .text_size(px(12.5))
                    .icon(icon(IconName::ChevronsUpDown, 12., palette.muted))
                    .accessibility_label(label)
                    .search_placeholder("搜索字体")
                    .menu_max_h(px(260.))
                    .empty(move |_, _| {
                        div()
                            .py(px(10.))
                            .text_center()
                            .text_color(palette.dim)
                            .child("未找到匹配字体")
                    }),
            )
    }
}

struct FontPickers {
    ui: FontPicker,
    mono: FontPicker,
}

fn setting_note(value: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .truncate()
        .text_size(px(12.))
        .text_color(palette.dim)
        .child(value.into())
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
fn terminal_line_spacing(settings: &Settings) -> f32 {
    let size = settings.terminal_font_size.clamp(8., 32.);
    settings.terminal_line_height.clamp(size, 56.) / size
}

fn appearance(
    settings: &Settings,
    view: &View,
    palette: Palette,
    fonts: &FontPickers,
    show_caption: bool,
) -> Div {
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
    let font_size = settings.terminal_font_size.clamp(8., 32.);
    let font_size_label = format!("{font_size:.2}");
    let font_size_label = font_size_label.trim_end_matches('0').trim_end_matches('.');
    let line_spacing = terminal_line_spacing(settings);
    let spacing_choices = [10, 12, 14, 15, 17, 20]
        .into_iter()
        .filter(|&spacing| font_size * (spacing as f32 / 10.) <= 56.);
    col()
        .flex_shrink_0()
        .child(
            settings_section(
                "主题",
                "深色、浅色两套主题等价设计，终端配色、图表和代码高亮随主题一起切换。",
                palette,
            )
            .child(theme_cards(settings, view, palette, show_caption)),
        )
        .child(
            settings_section("字体", "", palette)
                .border_t_1()
                .child(setting_line(
                    "界面字体",
                    "标签、菜单与表单。",
                    fonts.ui.render("set-ui-font", "界面字体", palette),
                    palette,
                ))
                .child(setting_line(
                    "终端与编辑器字体",
                    "建议使用中英文 2:1 等宽的字体，TUI 边框与表格才不会错位。",
                    fonts
                        .mono
                        .render("set-mono-font", "终端与编辑器字体", palette),
                    palette,
                ))
                .child(setting_line(
                    "终端字号与行高",
                    "",
                    row()
                        .gap(px(8.))
                        .child(choices(
                            "set-terminal-size",
                            font_size_label.to_owned(),
                            number_options([10, 11, 12, 13, 14, 15, 16, 18, 20], |size| {
                                size.to_string()
                            }),
                            80.,
                            palette,
                            view,
                            |settings, size| {
                                let spacing = terminal_line_spacing(settings);
                                settings.terminal_font_size = size as f32;
                                settings.terminal_line_height = (size as f32 * spacing).min(56.);
                            },
                        ))
                        .child(choices(
                            "set-terminal-spacing",
                            format!("{line_spacing:.1} 倍"),
                            number_options(spacing_choices, |spacing| {
                                format!("{:.1} 倍", spacing as f32 / 10.)
                            }),
                            100.,
                            palette,
                            view,
                            |settings, spacing| {
                                let size = settings.terminal_font_size.clamp(8., 32.);
                                settings.terminal_line_height = size * (spacing as f32 / 10.);
                            },
                        )),
                    palette,
                )),
        )
        .child(
            settings_section("行号与时间", "", palette)
                .border_t_1()
                .pb(px(16.))
                .child(setting_line(
                    "显示行号",
                    "按会话累计编号，全屏程序期间显示屏幕行号。",
                    toggle_setting(
                        "set-line-numbers",
                        settings.show_line_numbers,
                        view,
                        |s, v| s.show_line_numbers = v,
                    ),
                    palette,
                ))
                .child(setting_line(
                    "显示时间戳",
                    "每一行记录写入终端的时间；重绘行在滚入历史后定格。",
                    toggle_setting("set-timestamps", settings.show_timestamps, view, |s, v| {
                        s.show_timestamps = v
                    }),
                    palette,
                ))
                .child(setting_line(
                    "时间格式",
                    "",
                    choices(
                        "set-timestamp-format",
                        time_options[time_index].into(),
                        time_options
                            .into_iter()
                            .enumerate()
                            .map(|(index, text)| (index as u64, text.into()))
                            .collect(),
                        200.,
                        palette,
                        view,
                        |s, v| {
                            s.timestamp_format = match v {
                                1 => TimestampPreference::Milliseconds,
                                2 => TimestampPreference::Relative,
                                _ => TimestampPreference::Seconds,
                            }
                        },
                    ),
                    palette,
                ))
                .child(setting_line(
                    "弱化同一秒内重复的时间",
                    "让命令的开始时间更容易被扫到。",
                    toggle_setting(
                        "set-dim-timestamps",
                        settings.dim_repeated_timestamps,
                        view,
                        |s, v| s.dim_repeated_timestamps = v,
                    ),
                    palette,
                )),
        )
}

fn linux_highlight_settings(settings: &Settings, view: &View, palette: Palette) -> Div {
    let actions_view = view.clone();
    let rule_path = config::linux_highlights_path()
        .map(|path| config::display_path(&path.to_string_lossy()))
        .unwrap_or_else(|_| "linux-highlights.json".into());
    settings_section(
        "Linux 文本高亮",
        "在 SSH 与 WSL 终端中强调 Linux 命令输出中的关键文本，可编辑规则后重载。",
        palette,
    )
    .border_t_1()
    .pb(px(16.))
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
        setting_note(rule_path, palette),
        palette,
    ))
}

fn terminal_settings(
    settings: &Settings,
    view: &View,
    idle_input: &IdleInput,
    palette: Palette,
) -> Div {
    let shift_enter = view.clone();
    let notifications = view.clone();
    let osc52 = view.clone();
    let shift_options = [
        (ShiftEnterPreference::EscapeEnter, "发送 ESC + Enter"),
        (ShiftEnterPreference::Enter, "作为普通 Enter"),
        (ShiftEnterPreference::LineFeed, "发送换行符（LF）"),
    ];
    let notification_options = [
        (TerminalNotificationPreference::Off, "关闭"),
        (TerminalNotificationPreference::InApp, "仅应用内"),
        (
            TerminalNotificationPreference::InAppAndSystem,
            "应用内与系统",
        ),
    ];
    col()
    .flex_shrink_0()
    .child(
        settings_section(
            "终端兼容",
            "适用于 vim、htop、tmux、Claude Code、Codex、Gemini CLI 等程序。",
            palette,
        )
        .child(setting_line(
            "终端类型 TERM",
            "用于新的 SSH 连接和重连，可在单个会话中覆盖。发送 COLORTERM=truecolor 需服务器放行。",
            ssh_value_field(
                "set-ssh-term",
                settings.ssh_terminal_type.as_deref().filter(|value| *value != config::DEFAULT_TERMINAL_TYPE),
                &[
                    (None, config::DEFAULT_TERMINAL_TYPE),
                    (Some("xterm"), "xterm"),
                    (Some("vt100"), "vt100"),
                ],
                view,
                palette,
                |settings, value| settings.ssh_terminal_type = value,
            ),
            palette,
        ))
        .child(setting_line(
            "Shift+Enter",
            "终端程序协商 Kitty 键盘协议后，使用协议定义的编码。",
            preference_field(
                "set-shift-enter",
                settings.shift_enter,
                &shift_options,
                170.,
                palette,
                move |value, cx| {
                    let _ = shift_enter.update(cx, |this, cx| {
                        let mut settings = this.settings.clone();
                        settings.shift_enter = value;
                        if this.persist(settings, cx) {
                            this.apply_terminal_preferences(cx);
                        }
                    });
                },
            ),
            palette,
        ))
        .child(setting_line(
            "Kitty 键盘协议",
            "允许终端程序请求 CSI u 键盘编码。",
            toggle_setting("set-kitty-keyboard", settings.kitty_keyboard, view, |s, v| {
                s.kitty_keyboard = v;
            }),
            palette,
        ))
        .child(setting_line(
            "同步输出（DEC 2026）",
            "允许终端程序整帧刷新，减少闪烁。",
            toggle_setting(
                "set-synchronized-output",
                settings.synchronized_output,
                view,
                |s, v| s.synchronized_output = v,
            ),
            palette,
        ))
        .child(setting_line(
            "允许程序写入剪贴板（OSC 52）",
            "控制 Codex 等终端程序向本机剪贴板写入文本的请求。",
            preference_field(
                "set-osc52-write",
                settings.osc52_write,
                &[
                    (Osc52WritePreference::Ask, "每次询问"),
                    (Osc52WritePreference::Allow, "始终允许"),
                    (Osc52WritePreference::Deny, "拒绝"),
                ],
                160.,
                palette,
                move |value, cx| {
                    let _ = osc52.update(cx, |this, cx| {
                        let mut settings = this.settings.clone();
                        settings.osc52_write = value;
                        if this.persist(settings, cx) {
                            this.apply_terminal_preferences(cx);
                        }
                    });
                },
            ),
            palette,
        ))
        .child(setting_line(
            "桌面通知（OSC 9 / 777、响铃）",
            "后台标签需要确认时提醒。窗口失焦时可发送系统通知。",
            preference_field(
                "set-terminal-notifications",
                settings.terminal_notifications,
                &notification_options,
                170.,
                palette,
                move |value, cx| {
                    let _ = notifications.update(cx, |this, cx| {
                        let mut settings = this.settings.clone();
                        settings.terminal_notifications = value;
                        this.persist(settings, cx);
                    });
                },
            ),
            palette,
        ))
        .child(setting_line(
            "粘贴多行内容时确认",
            "直接粘贴到未启用括号粘贴的终端时生效。",
            toggle_setting(
                "set-confirm-multiline-paste",
                settings.confirm_multiline_paste,
                view,
                |s, value| s.confirm_multiline_paste = value,
            ),
            palette,
        ))
        .child(setting_line(
            "回滚行数",
            "0 表示关闭历史，修改后对新打开的终端标签生效。",
            choices(
                "set-scrollback-lines",
                scrollback_label(settings.scrollback_lines()),
                number_options(
                    [0, 1_000, 5_000, 10_000, 20_000, 50_000, config::MAX_SCROLLBACK_LINES],
                    scrollback_label,
                ),
                160.,
                palette,
                view,
                |settings, lines| settings.terminal_scrollback_lines = lines,
            ),
            palette,
        )),
    )
    .child(linux_highlight_settings(settings, view, palette))
    .child(
        settings_section("终端显示", "", palette)
            .border_t_1()
            .pb(px(16.))
            .child(setting_line(
                "显示空闲间隔线",
                "在相邻两行相隔较久时标注空闲时长。",
                toggle_setting(
                    "set-show-idle-separator",
                    settings.show_idle_separator,
                    view,
                    |settings, value| settings.show_idle_separator = value,
                ),
                palette,
            ))
            .when(settings.show_idle_separator, |section| {
                section.child(setting_line(
                    "空闲间隔线阈值",
                    "输入秒数后按 Enter 或离开输入框保存。",
                    idle_input.render(palette),
                    palette,
                ))
            }),
    )
}

fn ssh_settings(settings: &Settings, view: &View, palette: Palette, mono: SharedString) -> Div {
    let trust_view = view.clone();
    let session_view = view.clone();
    let mut agents = vec![
        (
            None,
            if cfg!(windows) {
                "Windows OpenSSH Agent"
            } else {
                "SSH_AUTH_SOCK"
            },
        ),
        (Some("none"), "不使用"),
    ];
    if cfg!(windows) {
        agents.insert(1, (Some("SSH_AUTH_SOCK"), "SSH_AUTH_SOCK"));
    }
    let agent = match settings.ssh_identity_agent.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("none") => Some("none"),
        Some("SSH_AUTH_SOCK") if !cfg!(windows) => None,
        value => value,
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
            settings_section("已保存的会话", "", palette).child(setting_line(
                "打开方式",
                "双击右侧会话列表中的条目，或点击条目上的连接按钮时使用。",
                preference_field(
                    "set-saved-session-open-mode",
                    settings.open_saved_session_in_new_tab,
                    &[(false, "切换到已打开的标签"), (true, "在新标签中连接")],
                    180.,
                    palette,
                    move |value, cx| {
                        let _ = session_view.update(cx, |this, cx| {
                            let mut settings = this.settings.clone();
                            settings.open_saved_session_in_new_tab = value;
                            this.persist(settings, cx);
                        });
                    },
                ),
                palette,
            )),
        )
        .child(
            settings_section(
                "SSH 配置文件",
                "按照添加顺序显示各配置文件与已同步会话。",
                palette,
            )
            .border_t_1()
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
                    "适用于目标主机和跳板。",
                    preference_field(
                        "set-require-known-host",
                        settings.ssh_require_known_host,
                        &[(false, "显示指纹并询问"), (true, "仅允许已知主机")],
                        180.,
                        palette,
                        move |value, cx| {
                            let _ = trust_view.update(cx, |this, cx| {
                                let mut settings = this.settings.clone();
                                settings.ssh_require_known_host = value;
                                this.persist(settings, cx);
                            });
                        },
                    ),
                    palette,
                ))
                .child(setting_line(
                    "SSH Agent",
                    "用于新的 SSH 连接和重连，可在会话的认证设置中覆盖。",
                    ssh_value_field(
                        "set-ssh-agent",
                        agent,
                        &agents,
                        view,
                        palette,
                        |settings, value| settings.ssh_identity_agent = value,
                    ),
                    palette,
                ))
                .child(setting_line(
                    "连接超时",
                    "每一跳单独计时。",
                    choices(
                        "set-ssh-timeout",
                        seconds_label(u64::from(settings.ssh_timeout.max(1))),
                        number_options([5, 10, 15, 30, 60], seconds_label),
                        120.,
                        palette,
                        view,
                        |settings, value| settings.ssh_timeout = value as u32,
                    ),
                    palette,
                ))
                .child(setting_line(
                    "保活间隔",
                    "使用 SSH 协议保活消息。",
                    choices(
                        "set-ssh-keepalive",
                        keepalive_label(u64::from(settings.ssh_keepalive)),
                        number_options([0, 15, 30, 60, 120], keepalive_label),
                        120.,
                        palette,
                        view,
                        |settings, value| settings.ssh_keepalive = value as u32,
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
    let downloads = settings.download_dir();
    let choice = view.clone();
    let reset = view.clone();
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
                        .child(config::display_path(&downloads.to_string_lossy())),
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
                )
                .when(!settings.download_directory.is_empty(), |row| {
                    row.child(
                        button("reset-download-location", "恢复默认", palette)
                            .h(px(22.))
                            .text_size(px(11.5))
                            .on_click(move |_, _, cx| {
                                let _ = reset.update(cx, |this, cx| {
                                    let mut settings = this.settings.clone();
                                    settings.download_directory.clear();
                                    this.persist(settings, cx);
                                });
                            }),
                    )
                }),
            palette,
        ))
        .child(setting_line(
            "同时传输",
            "",
            choices(
                "set-transfer-concurrency",
                format!("{} 个文件", settings.transfer_concurrency.clamp(1, 16)),
                number_options([1, 2, 3, 4, 5, 8], |value| format!("{value} 个文件")),
                120.,
                palette,
                view,
                |settings, value| settings.transfer_concurrency = value as usize,
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
                format!("{} MB", settings.file_edit_limit_mb.clamp(1, 50)),
                number_options([1, 2, 5, 10, 20, 50], |value| format!("{value} MB")),
                120.,
                palette,
                view,
                |settings, value| settings.file_edit_limit_mb = value,
            ),
            palette,
        ))
        .child(setting_line(
            "保存方式",
            "远程文件先写入同目录的临时文件。",
            setting_note("原子替换", palette),
            palette,
        ))
}

fn monitor_settings(settings: &Settings, view: &View, palette: Palette) -> Div {
    let warn = u64::from(settings.monitor_warn_pct);
    let danger = u64::from(settings.monitor_danger_pct);
    let thresholds = [50, 60, 70, 75, 80, 85, 90, 95, 98, 100];
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
            seconds_label(settings.monitor_interval.clamp(1, 60)),
            number_options([2, 5, 10, 15, 30], seconds_label),
            120.,
            palette,
            view,
            |settings, value| settings.monitor_interval = value,
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
                format!("{warn}%"),
                number_options(
                    thresholds.into_iter().filter(|&value| value < danger),
                    |value| format!("{value}%"),
                ),
                90.,
                palette,
                view,
                |settings, value| settings.monitor_warn_pct = value as u8,
            ))
            .child(choices(
                "set-monitor-danger",
                format!("{danger}%"),
                number_options(
                    thresholds.into_iter().filter(|&value| value > warn),
                    |value| format!("{value}%"),
                ),
                90.,
                palette,
                view,
                |settings, value| settings.monitor_danger_pct = value as u8,
            )),
        palette,
    ))
    .child(setting_line(
        "窗口在后台时暂停采样",
        "",
        toggle_setting(
            "set-monitor-pause-in-background",
            settings.monitor_pause_in_background,
            view,
            |settings, value| settings.monitor_pause_in_background = value,
        ),
        palette,
    ))
}

impl Workbench {
    pub(in crate::workbench) fn settings_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = Rc::new(Cell::new(0));
        let view = cx.weak_entity();
        let (shortcut_recorder, shortcut_keys) = shortcut_editor::ShortcutRecorder::new(cx);
        let idle_input = IdleInput::new(&self.settings, &view, window, cx);
        let installed = cx.text_system().all_font_names();
        let fonts = FontPickers {
            ui: FontPicker::new(FontRole::Ui, &installed, window, cx),
            mono: FontPicker::new(FontRole::Mono, &installed, window, cx),
        };
        let path = if cfg!(windows) {
            r"%APPDATA%\CapTerm\settings.json".to_owned()
        } else {
            config::settings_path()
                .map(|path| config::display_path(&path.to_string_lossy()))
                .unwrap_or_else(|_| "settings.json".into())
        };
        window.open_dialog(cx, move |dialog, window, cx| {
            let _ = &shortcut_keys;
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
                    let shortcut_recorder = shortcut_recorder.clone();
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
                            shortcut_recorder.cancel();
                            selected.set(index);
                            window.refresh();
                        })
                }));
            let theme = Theme::global(cx);
            let body = match selected.get() {
                0 => appearance(settings, &view, palette, &fonts, width >= px(720.)),
                1 => terminal_settings(settings, &view, &idle_input, palette),
                2 => ssh_settings(settings, &view, palette, theme.mono_font_family.clone()),
                3 => transfer_settings(settings, &view, palette, theme.mono_font_family.clone()),
                4 => monitor_settings(settings, &view, palette),
                _ => shortcut_recorder.render(settings, &view, palette, window),
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
