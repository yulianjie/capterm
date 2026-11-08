use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{Local, NaiveTime};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{Disableable, Theme};
use gpui_kit::prelude::*;
use gpui_kit::*;
use tokio::{sync::watch, time};

use super::{
    Event, Phase, SendTarget, Tab, Workbench,
    ui::{Palette, col, icon, icon_tool, row, tool},
};
use crate::ssh::{Connection, Metrics, ProcessSample, Usage};

const TREND_SPAN: Duration = Duration::from_secs(5 * 60);

pub(super) async fn sample_host(
    id: u64,
    connection: Arc<Connection>,
    mut monitored_operation: watch::Receiver<Option<u64>>,
    interval: Arc<AtomicU64>,
    events: async_channel::Sender<Event>,
) {
    let mut next = time::Instant::now();
    while monitored_operation
        .wait_for(|active| *active == Some(id))
        .await
        .is_ok()
    {
        tokio::select! {
            _ = time::sleep_until(next) => {
                if *monitored_operation.borrow_and_update() != Some(id) {
                    continue;
                }
            }
            changed = monitored_operation.changed() => {
                if changed.is_err() {
                    break;
                }
                continue;
            }
        }
        let result = connection.metrics().await;
        next = time::Instant::now() + Duration::from_secs(interval.load(Ordering::Relaxed));
        if let Ok(sample) = result
            && events.send(Event::Sample(id, sample)).await.is_err()
        {
            break;
        }
    }
}

#[derive(Default)]
pub(super) struct MonitorState {
    pub(super) paused: bool,
    processes: bool,
    latest: Option<Arc<Metrics>>,
    history: VecDeque<SamplePoint>,
    selected: [Option<Instant>; 3],
    focus: RefCell<[Option<FocusHandle>; 3]>,
    data_view: Option<Trend>,
}

#[derive(Clone, Copy)]
struct SamplePoint {
    at: Instant,
    time: NaiveTime,
    values: [Option<f64>; 3],
}

#[derive(Clone, Copy)]
struct ChartPoint {
    at: Instant,
    time: NaiveTime,
    value: f64,
}

#[derive(Clone, Copy)]
enum Trend {
    Cpu,
    Receive,
    Send,
}

impl Trend {
    fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Receive => "下行",
            Self::Send => "上行",
        }
    }

    fn value(self, value: f64) -> String {
        match self {
            Self::Cpu => format!("{value:.1}%"),
            Self::Receive | Self::Send => {
                let (number, unit) = format_network_rate(value);
                format!("{number} {unit}")
            }
        }
    }
}

fn format_network_rate(bytes_per_second: f64) -> (String, &'static str) {
    if bytes_per_second >= 1_000_000. {
        (format!("{:.1}", bytes_per_second / 1_000_000.), "MB/s")
    } else if bytes_per_second >= 1_000. {
        (format!("{:.1}", bytes_per_second / 1_000.), "KB/s")
    } else if bytes_per_second > 0. && bytes_per_second < 0.1 {
        ("<0.1".into(), "B/s")
    } else {
        (format!("{bytes_per_second:.1}"), "B/s")
    }
}

impl MonitorState {
    pub(super) fn push(&mut self, metrics: Arc<Metrics>) {
        if !metrics.probe_available {
            return;
        }
        let at = Instant::now();
        let (receive, send) = metrics.network_bps.unzip();
        self.history.push_back(SamplePoint {
            at,
            time: Local::now().time(),
            values: [metrics.cpu, receive, send],
        });
        while self
            .history
            .front()
            .is_some_and(|sample| at.duration_since(sample.at) > TREND_SPAN)
        {
            self.history.pop_front();
        }
        self.latest = Some(metrics);
    }

    pub(super) fn clear(&mut self) {
        self.history.clear();
        self.latest = None;
        self.selected.fill(None);
        self.data_view = None;
    }

    fn trend(&self, trend: Trend) -> Vec<ChartPoint> {
        self.history
            .iter()
            .filter_map(|sample| {
                Some(ChartPoint {
                    at: sample.at,
                    time: sample.time,
                    value: sample.values[trend.index()]?,
                })
            })
            .collect()
    }
}

impl Workbench {
    pub(super) fn host_monitor(
        &self,
        palette: Palette,
        mono: SharedString,
        height: Pixels,
        _window: &mut Window,
        cx: &Context<Self>,
    ) -> Div {
        let tab = self.active_tab();
        let connected = tab.is_some_and(|tab| tab.phase == Phase::Connected && !tab.is_local());
        let ssh_tab = tab.filter(|tab| !tab.is_local());
        let tab_id = ssh_tab.map(|tab| tab.id);
        let state = ssh_tab.map(|tab| &tab.monitor_state);
        let paused = state.is_some_and(|state| state.paused);
        let mut pages = row()
            .h(px(24.))
            .gap(px(2.))
            .p(px(2.))
            .flex_shrink_0()
            .bg(palette.hover)
            .rounded(px(6.));
        for (processes, label) in [(false, "概览"), (true, "进程")] {
            pages = pages.child(
                tool(
                    ("monitor-page", u64::from(processes)),
                    label,
                    label,
                    palette,
                )
                .h(px(20.))
                .min_w(px(36.))
                .px(px(8.))
                .text_size(px(11.5))
                .rounded(px(4.))
                .disabled(tab_id.is_none())
                .when(
                    state.is_some_and(|state| state.processes) == processes,
                    |button| button.bg(palette.raised).text_color(palette.ink),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(tab) = tab_id.and_then(|id| this.tab_mut(id)) {
                        tab.monitor_state.processes = processes;
                        tab.monitor_state.data_view = None;
                        cx.notify();
                    }
                })),
            );
        }
        let head = row()
            .h(px(34.))
            .flex_shrink_0()
            .pl(px(12.))
            .pr(px(6.))
            .gap(px(4.))
            .border_b_1()
            .border_color(palette.line)
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("监控"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .pl(px(4.))
                    .text_size(px(11.5))
                    .text_color(palette.dim)
                    .child(ssh_tab.map_or("", Tab::name).to_owned()),
            )
            .child(pages)
            .child(
                icon_tool(
                    "monitor-pause",
                    if paused {
                        IconName::Play
                    } else {
                        IconName::Pause
                    },
                    if paused {
                        "继续采样"
                    } else {
                        "暂停采样"
                    },
                    palette,
                )
                .h(px(24.))
                .min_w(px(24.))
                .disabled(!connected)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(tab) = tab_id.and_then(|id| this.tab_mut(id)) {
                        tab.monitor_state.paused = !tab.monitor_state.paused;
                        this.set_monitored_tab(this.active);
                        cx.notify();
                    }
                })),
            );

        let body = match ssh_tab.filter(|_| connected).and_then(|tab| {
            tab.monitor_state
                .latest
                .as_ref()
                .map(|metrics| (tab, metrics))
        }) {
            Some((_, metrics)) if metrics.unsupported => empty_state(
                palette,
                "暂不支持该系统",
                metrics.os.as_deref().unwrap_or("无法识别主机系统"),
            ),
            Some((tab, metrics)) => {
                let state = &tab.monitor_state;
                if let Some(trend) = state.data_view {
                    self.monitor_data(tab, trend, palette, cx)
                } else if state.processes {
                    self.monitor_processes(tab, metrics, palette, mono, cx)
                } else {
                    self.monitor_overview(tab, metrics, palette, mono, cx)
                }
            }
            None if ssh_tab.is_some_and(|tab| tab.phase == Phase::Connecting) => empty_state(
                palette,
                "连接建立后开始采样",
                "通过当前 SSH 连接采集主机数据，无需在服务器安装程序。",
            ),
            None if connected => {
                let status = if paused {
                    "采样已暂停"
                } else if ssh_tab.is_some_and(|tab| tab.sample.is_some()) {
                    "当前主机无法执行采集命令"
                } else {
                    "等待首次采样"
                };
                empty_state(palette, status, "通过当前 SSH 连接采集主机数据。")
            }
            _ => empty_state(
                palette,
                "没有监控数据",
                "打开一个已连接的会话即可查看 CPU、内存、磁盘和网络。",
            ),
        };

        col()
            .h(height)
            .flex_shrink_0()
            .border_t_1()
            .border_color(palette.line)
            .bg(palette.panel)
            .child(head)
            .child(
                col()
                    .id("host-monitor-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }

    fn monitor_overview(
        &self,
        tab: &Tab,
        metrics: &Metrics,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let cpu = metrics.cpu.map_or("—".into(), |cpu| format!("{cpu:.0}%"));
        let cores = metrics
            .cores
            .map_or("近 5 分钟".into(), |n| format!("{n} 核，5 分钟"));
        let thresholds = (
            f64::from(self.settings.monitor_warn_pct),
            f64::from(self.settings.monitor_danger_pct),
        );
        let (cpu_color, cpu_status) =
            resource_severity(metrics.cpu.unwrap_or(0.), thresholds, palette);
        let uptime = metrics.uptime_secs.map(|secs| {
            if secs >= 86_400 {
                format!("已运行 {} 天", secs / 86_400)
            } else if secs >= 3600 {
                format!("已运行 {} 小时", secs / 3600)
            } else if secs >= 60 {
                format!("已运行 {} 分钟", secs / 60)
            } else {
                format!("已运行 {secs} 秒")
            }
        });
        let mut sys = row()
            .flex_wrap()
            .gap_x(px(10.))
            .text_size(px(11.5))
            .line_height(px(16.))
            .text_color(palette.dim);
        if let Some(os) = &metrics.os {
            sys = sys.child(
                div()
                    .text_color(palette.muted)
                    .font_weight(FontWeight::MEDIUM)
                    .child(os.clone()),
            );
        }
        if let Some(kernel) = &metrics.kernel {
            let kernel = kernel
                .split_once('-')
                .map_or(kernel.as_str(), |(version, _)| version);
            sys = sys.child(format!("内核 {kernel}"));
        }
        if let Some(uptime) = uptime {
            sys = sys.child(uptime);
        }
        let mut body = monitor_body().gap(px(11.)).child(sys).child(
            row()
                .gap(px(10.))
                .child(
                    col()
                        .w(px(74.))
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(palette.muted)
                        .child(
                            row()
                                .gap(px(4.))
                                .child("CPU")
                                .when_some(cpu_status, |row, status| {
                                    row.child(severity_tag(cpu_color, status))
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(18.))
                                .line_height(px(24.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.ink)
                                .child(cpu),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .line_height(px(14.))
                                .text_color(palette.dim)
                                .child(cores),
                        ),
                )
                .child(div().flex_1().min_w_0().child(self.sparkline(
                    tab,
                    Trend::Cpu,
                    px(42.),
                    palette,
                    cx,
                ))),
        );
        if metrics.memory.is_some() || metrics.swap.is_some() {
            body = body.child(
                col().gap(px(8.)).children(
                    [("内存", metrics.memory), ("交换", metrics.swap)]
                        .into_iter()
                        .filter_map(|(label, usage)| {
                            Some(meter(label, None, usage?, thresholds, palette, &mono))
                        }),
                ),
            );
        }
        if !metrics.disks.is_empty() {
            body = body.child(col().gap(px(8.)).children(metrics.disks.iter().map(|disk| {
                meter(
                    "磁盘",
                    Some(disk.mount.as_str()),
                    disk.usage,
                    thresholds,
                    palette,
                    &mono,
                )
            })));
        }
        body = body.child(
            row()
                .items_start()
                .gap(px(12.))
                .child(self.network_trend(tab, Trend::Receive, palette, cx))
                .child(self.network_trend(tab, Trend::Send, palette, cx)),
        );
        if let Some([one, five, fifteen]) = metrics.load {
            body = body.child(
                row()
                    .items_baseline()
                    .gap(px(12.))
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(palette.muted)
                    .child("负载")
                    .children([one, five, fifteen].into_iter().map(|load| {
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.ink)
                            .child(format!("{load:.2}"))
                    }))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(palette.dim)
                            .child("1 / 5 / 15 分钟"),
                    ),
            );
        }
        body
    }

    fn network_trend(&self, tab: &Tab, trend: Trend, palette: Palette, cx: &Context<Self>) -> Div {
        let (current, unit) = tab
            .monitor_state
            .latest
            .as_ref()
            .and_then(|metrics| metrics.network_bps)
            .map(|(rx, tx)| {
                let rate = if matches!(trend, Trend::Receive) {
                    rx
                } else {
                    tx
                };
                format_network_rate(rate)
            })
            .unwrap_or_else(|| ("—".into(), ""));
        let peak = tab
            .monitor_state
            .trend(trend)
            .iter()
            .map(|sample| sample.value)
            .reduce(f64::max);
        col()
            .flex_1()
            .text_size(px(11.))
            .text_color(palette.dim)
            .child(
                row()
                    .items_baseline()
                    .h(px(18.))
                    .flex_shrink_0()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(palette.muted)
                    .child(icon(
                        if matches!(trend, Trend::Receive) {
                            IconName::ArrowDown
                        } else {
                            IconName::ArrowUp
                        },
                        12.,
                        palette.muted,
                    ))
                    .child(div().flex_shrink_0().child(trend.label()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(palette.ink)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(current),
                    )
                    .when(!unit.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(palette.dim)
                                .child(unit),
                        )
                    }),
            )
            .child(
                div()
                    .mt(px(4.))
                    .child(self.sparkline(tab, trend, px(30.), palette, cx)),
            )
            .child(div().mt(px(2.)).line_height(px(16.)).child(format!(
                "峰值 {}",
                peak.map_or("—".into(), |n| trend.value(n))
            )))
    }

    fn sparkline(
        &self,
        tab: &Tab,
        trend: Trend,
        height: Pixels,
        palette: Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let id = tab.id;
        let state = &tab.monitor_state;
        let index = trend.index();
        let focus = state.focus.borrow_mut()[index]
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let bounds = Rc::new(Cell::new(Bounds::default()));
        let paint_bounds = bounds.clone();
        let samples = Rc::new(state.trend(trend));
        let points = samples.clone();
        let selected = state.selected[index]
            .and_then(|at| points.iter().find(|point| point.at == at))
            .copied();
        let end = state
            .history
            .back()
            .map(|point| point.at)
            .unwrap_or_else(Instant::now);
        let menu_view = cx.weak_entity();
        row()
            .id(("monitor-chart", index))
            .relative()
            .w_full()
            .h(height)
            .flex_shrink_0()
            .track_focus(&focus)
            .tab_stop(true)
            .aria_label(format!(
                "{}趋势，近 5 分钟，使用方向键查看采样",
                trend.label()
            ))
            .focus(|style| style.border_1().border_color(palette.accent))
            .cursor(CursorStyle::Crosshair)
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let bounds = bounds.get();
                let distance = |sample: &ChartPoint| {
                    f32::from((chart_x(bounds, end, sample.at) - event.position.x).abs())
                };
                let closest = samples
                    .iter()
                    .min_by(|a, b| distance(a).total_cmp(&distance(b)));
                this.select_chart_point(id, trend, closest.map(|sample| sample.at), cx);
            }))
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if !hovering {
                    this.select_chart_point(id, trend, None, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus.focus(window, cx);
            })
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                let Some(tab) = this.tab_mut(id) else { return };
                let state = &mut tab.monitor_state;
                if key == "enter" {
                    state.data_view = Some(trend);
                } else if matches!(key, "left" | "right") {
                    let points = state.trend(trend);
                    let current = state.selected[index]
                        .and_then(|at| points.iter().position(|point| point.at == at));
                    let next = current.map_or(points.len().saturating_sub(1), |position| {
                        if key == "left" {
                            position.saturating_sub(1)
                        } else {
                            position + 1
                        }
                    });
                    state.selected[index] = points
                        .get(next)
                        .map(|point| point.at)
                        .or_else(|| points.last().map(|point| point.at));
                } else {
                    return;
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                canvas(
                    move |bounds, _, _| paint_bounds.set(bounds),
                    move |bounds, _, window, cx| {
                        paint_trend(bounds, &points, trend, selected, end, window, cx);
                    },
                )
                .size_full(),
            )
            .when_some(selected, |chart, point| {
                let fraction = chart_fraction(end, point.at);
                let offset = if fraction > 0.5 {
                    fraction - 0.66
                } else {
                    fraction + 0.06
                };
                chart.child(
                    col()
                        .absolute()
                        .top_0()
                        .left(relative(offset.clamp(0., 0.40)))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .shadow_sm()
                        .bg(palette.raised)
                        .text_size(px(10.))
                        .line_height(px(13.))
                        .text_color(palette.dim)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.ink)
                                .child(trend.value(point.value)),
                        )
                        .child(format!(
                            "{} {}",
                            trend.label(),
                            point.time.format("%H:%M:%S")
                        )),
                )
            })
            .context_menu(move |menu, _, _| {
                let view = menu_view.clone();
                menu.item(PopupMenuItem::new("查看数据").on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if let Some(tab) = this.tab_mut(id) {
                            tab.monitor_state.data_view = Some(trend);
                            cx.notify();
                        }
                    });
                }))
            })
    }

    fn select_chart_point(
        &mut self,
        id: u64,
        trend: Trend,
        at: Option<Instant>,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tab_mut(id) {
            let selection = &mut tab.monitor_state.selected[trend.index()];
            if *selection != at {
                *selection = at;
                cx.notify();
            }
        }
    }

    fn monitor_data(&self, tab: &Tab, trend: Trend, palette: Palette, cx: &Context<Self>) -> Div {
        let id = tab.id;
        monitor_body()
            .gap(px(2.))
            .child(
                row()
                    .justify_between()
                    .text_size(px(12.))
                    .child(format!("{} · 近 5 分钟", trend.label()))
                    .child(
                        tool("monitor-data-back", "‹ 返回", "返回监控概览", palette)
                            .h(px(22.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(tab) = this.tab_mut(id) {
                                    tab.monitor_state.data_view = None;
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(table_line("采样时间", "数值", palette))
            .children(
                tab.monitor_state
                    .trend(trend)
                    .into_iter()
                    .rev()
                    .map(|point| {
                        table_line(
                            point.time.format("%H:%M:%S").to_string(),
                            trend.value(point.value),
                            palette,
                        )
                    }),
            )
    }

    fn monitor_processes(
        &self,
        tab: &Tab,
        metrics: &Metrics,
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let id = tab.id;
        let count = metrics.processes.len();
        let peak = metrics
            .processes
            .first()
            .map_or(1., |sample| sample.cpu.max(1.));
        let agent = tab
            .terminal
            .read(cx)
            .agent_name()
            .and_then(|name| name.split_whitespace().next())
            .map(str::to_ascii_lowercase);
        let time = tab
            .monitor_state
            .history
            .back()
            .map(|point| point.time.format("%H:%M:%S").to_string())
            .unwrap_or_default();
        monitor_body()
            .gap(px(0.))
            .child(
                row()
                    .pb(px(8.))
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(palette.muted)
                    .child(format!("按 CPU 排序，共 {count} 个进程"))
                    .child(time),
            )
            .child(
                row()
                    .h(px(24.))
                    .border_b_1()
                    .border_color(palette.line)
                    .text_size(px(11.))
                    .text_color(palette.muted)
                    .child(div().w(px(44.)).child("PID"))
                    .child(div().flex_1().child("命令"))
                    .child(div().w(px(70.)).text_right().child("CPU"))
                    .child(div().w(px(44.)).text_right().child("内存")),
            )
            .children(metrics.processes.iter().map(|process| {
                let command = process
                    .command
                    .rsplit('/')
                    .next()
                    .unwrap_or(&process.command);
                let highlighted = agent
                    .as_ref()
                    .is_some_and(|name| command.eq_ignore_ascii_case(name));
                self.process_row(id, process, (peak, highlighted), palette, mono.clone(), cx)
            }))
            .child(
                div()
                    .pt(px(8.))
                    .text_size(px(11.))
                    .text_color(palette.muted)
                    .child("右键进程可复制 PID、准备发送信号或在终端中查看详情。"),
            )
    }

    fn process_row(
        &self,
        id: u64,
        process: &ProcessSample,
        (peak, highlighted): (f64, bool),
        palette: Palette,
        mono: SharedString,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let pid = process.pid;
        let view = cx.weak_entity();
        row()
            .id(("monitor-process", u64::from(pid)))
            .h(px(26.))
            .border_b_1()
            .border_color(palette.line.opacity(0.6))
            .text_size(px(11.))
            .font_family(mono)
            .when(highlighted, |row| row.bg(palette.accent.opacity(0.12)))
            .hover(|style| style.bg(palette.raised))
            .child(
                div()
                    .w(px(44.))
                    .text_color(palette.muted)
                    .child(pid.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(process.command.clone()),
            )
            .child(
                row()
                    .w(px(70.))
                    .gap(px(4.))
                    .justify_end()
                    .child(format!("{:.1}%", process.cpu))
                    .child(
                        div()
                            .w(px(19.))
                            .h(px(4.))
                            .bg(palette.accent.opacity(0.16))
                            .rounded(px(2.))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative((process.cpu / peak) as f32))
                                    .rounded(px(2.))
                                    .bg(palette.accent),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(44.))
                    .text_right()
                    .child(format!("{:.1}%", process.memory)),
            )
            .context_menu(move |menu, _, _| {
                let inspect = view.clone();
                let signal = view.clone();
                menu.item(PopupMenuItem::new("复制 PID").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(pid.to_string()));
                }))
                .item(
                    PopupMenuItem::new("在终端查看进程").on_click(move |_, window, cx| {
                        let _ = inspect.update(cx, |this, cx| {
                            this.stage_process_command(id, format!("ps -fp {pid}"), window, cx);
                        });
                    }),
                )
                .item(
                    PopupMenuItem::new("准备发送 SIGTERM").on_click(move |_, window, cx| {
                        let _ = signal.update(cx, |this, cx| {
                            this.stage_process_command(id, format!("kill -TERM {pid}"), window, cx);
                        });
                    }),
                )
            })
    }

    fn stage_process_command(
        &mut self,
        id: u64,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .active_tab()
            .filter(|tab| tab.id == id && tab.phase == Phase::Connected)
        else {
            return;
        };
        if !tab.draft.read(cx).value().is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
            self.announce("输入栏已有内容，命令已复制。", cx);
            return;
        }
        let draft = tab.draft.clone();
        self.send_target = SendTarget::Active;
        draft.update(cx, |draft, cx| {
            draft.set_value(command, window, cx);
        });
        self.focus_sender(window, cx);
    }
}

fn monitor_body() -> Div {
    col()
        .w_full()
        .flex_shrink_0()
        .gap(px(8.))
        .px(px(12.))
        .pt(px(10.))
        .pb(px(12.))
}

fn empty_state(palette: Palette, title: &str, hint: &str) -> Div {
    monitor_body()
        .items_center()
        .pt(px(22.))
        .text_center()
        .child(icon(IconName::Activity, 21., palette.dim))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(13.))
                .child(title.to_owned()),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(palette.muted)
                .child(hint.to_owned()),
        )
}

fn table_line(
    first: impl Into<SharedString>,
    second: impl Into<SharedString>,
    palette: Palette,
) -> Div {
    row()
        .h(px(24.))
        .justify_between()
        .border_b_1()
        .border_color(palette.line)
        .text_size(px(11.))
        .text_color(palette.muted)
        .child(first.into())
        .child(second.into())
}

fn meter(
    title: &'static str,
    mount: Option<&str>,
    usage: Usage,
    thresholds: (f64, f64),
    palette: Palette,
    mono: &SharedString,
) -> Div {
    let (color, status) = resource_severity(usage.percent, thresholds, palette);
    col()
        .gap(px(5.))
        .child(
            row()
                .gap(px(6.))
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(palette.muted)
                .child(div().flex_shrink_0().child(title))
                .when_some(mount, |row, mount| {
                    row.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(11.5))
                            .text_color(palette.ink)
                            .child(mount.to_owned()),
                    )
                })
                .when_some(status, |row, status| row.child(severity_tag(color, status)))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(palette.dim)
                        .child(usage_label(usage, mount.is_some())),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .min_w(px(30.))
                        .text_right()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.ink)
                        .child(format!("{:.0}%", usage.percent)),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(6.))
                .overflow_hidden()
                .rounded(px(3.))
                .bg(color.opacity(0.16))
                .child(
                    div()
                        .h_full()
                        .w(relative((usage.percent / 100.) as f32))
                        .rounded(px(3.))
                        .bg(color),
                ),
        )
}

fn resource_severity(
    percent: f64,
    (warn, danger): (f64, f64),
    palette: Palette,
) -> (Hsla, Option<(IconName, &'static str)>) {
    if percent >= danger {
        (palette.danger, Some((IconName::CircleAlert, "告急")))
    } else if percent >= warn {
        (palette.warn, Some((IconName::TriangleAlert, "偏高")))
    } else {
        (palette.accent, None)
    }
}

fn severity_tag(color: Hsla, (symbol, label): (IconName, &'static str)) -> Div {
    row()
        .flex_shrink_0()
        .gap(px(2.))
        .text_size(px(10.))
        .text_color(color)
        .child(icon(symbol, 11., color))
        .child(label)
}

fn usage_label(usage: Usage, disk: bool) -> String {
    let base = if disk { 1000u64 } else { 1024u64 };
    let units = if disk {
        ["B", "KB", "MB", "GB", "TB"]
    } else {
        ["B", "KiB", "MiB", "GiB", "TiB"]
    };
    let index = (1..units.len())
        .rev()
        .find(|&index| usage.total >= base.pow(index as u32))
        .unwrap_or(0);
    let unit = units[index];
    let scale = base.pow(index as u32) as f64;
    let format = |bytes| {
        let value = bytes as f64 / scale;
        if value == 0. || value >= 10. || index == 0 {
            format!("{value:.0}")
        } else {
            format!("{value:.1}")
        }
    };
    format!("{} / {} {unit}", format(usage.used), format(usage.total))
}

fn chart_x(bounds: Bounds<Pixels>, end: Instant, at: Instant) -> Pixels {
    bounds.origin.x + px(6.) + (bounds.size.width - px(12.)).max(px(0.)) * chart_fraction(end, at)
}

fn chart_fraction(end: Instant, at: Instant) -> f32 {
    (1. - end.duration_since(at).as_secs_f32() / TREND_SPAN.as_secs_f32()).clamp(0., 1.)
}

fn paint_trend(
    bounds: Bounds<Pixels>,
    samples: &[ChartPoint],
    trend: Trend,
    selected: Option<ChartPoint>,
    end: Instant,
    window: &mut Window,
    cx: &App,
) {
    if bounds.size.width < px(12.) || bounds.size.height < px(12.) {
        return;
    }
    let colors = &Theme::global(cx).colors;
    let bottom = bounds.bottom() - px(6.);
    window.paint_quad(fill(
        Bounds::new(
            point(bounds.left(), bottom),
            size(bounds.size.width, px(1.)),
        ),
        colors.chart_grid,
    ));
    let limit = if matches!(trend, Trend::Cpu) {
        100.
    } else {
        let peak = samples.iter().map(|sample| sample.value).fold(0., f64::max);
        if peak > 0. { peak * 1.18 } else { 1. }
    };
    let position = |sample: &ChartPoint| {
        let x = chart_x(bounds, end, sample.at);
        let y =
            bottom - (bounds.size.height - px(12.)).max(px(0.)) * ((sample.value / limit) as f32);
        point(x, y)
    };
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return;
    };
    let first = position(first);
    let last = position(last);
    if samples.len() > 1 {
        let mut area = PathBuilder::fill();
        area.move_to(point(first.x, bottom));
        area.line_to(first);
        let mut line = PathBuilder::stroke(px(2.));
        line.move_to(first);
        for sample in &samples[1..] {
            let point = position(sample);
            area.line_to(point);
            line.line_to(point);
        }
        area.line_to(point(last.x, bottom));
        area.close();
        if let Ok(path) = area.build() {
            window.paint_path(path, colors.chart_1.opacity(0.10));
        }
        if let Ok(path) = line.build() {
            window.paint_path(path, colors.chart_1);
        }
    }
    let dot = |point: Point<Pixels>| {
        quad(
            Bounds::new(
                point - gpui_kit::point(px(6.), px(6.)),
                size(px(12.), px(12.)),
            ),
            px(6.),
            colors.chart_1,
            px(2.),
            colors.sidebar,
            BorderStyle::default(),
        )
    };
    window.paint_quad(dot(last));
    if let Some(sample) = selected {
        let point = position(&sample);
        window.paint_quad(fill(
            Bounds::new(
                gpui_kit::point(point.x, bounds.top()),
                size(px(1.), bottom - bounds.top()),
            ),
            colors.tab_foreground,
        ));
        window.paint_quad(dot(point));
    }
}

#[cfg(test)]
mod tests {
    use super::Trend;

    #[test]
    fn network_values_keep_small_rates_visible() {
        assert_eq!(Trend::Receive.value(0.), "0.0 B/s");
        assert_eq!(Trend::Receive.value(0.025), "<0.1 B/s");
        assert_eq!(Trend::Receive.value(120.), "120.0 B/s");
        assert_eq!(Trend::Receive.value(4_700.), "4.7 KB/s");
        assert_eq!(Trend::Send.value(1_200_000.), "1.2 MB/s");
    }
}
