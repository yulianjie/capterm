use std::collections::HashSet;

use anyhow::{anyhow, ensure};
use gpui_kit::component::button::{Button, ButtonVariants, DropdownButton};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable, Theme};

use super::*;
use crate::config::{self, Hop, SshOptions};

const NAV: [(IconName, &str); 6] = [
    (IconName::Server, "基本信息"),
    (IconName::KeyRound, "认证"),
    (IconName::Waypoints, "跳板与代理"),
    (IconName::SquareTerminal, "终端"),
    (IconName::ArrowLeftRight, "端口转发"),
    (IconName::Settings, "高级"),
];

struct HopForm {
    name: Option<String>,
    address: Entity<InputState>,
    identities: Entity<InputState>,
    ssh: SshOptions,
    reference: Option<HopReference>,
    editing: Cell<bool>,
    _subscriptions: [Subscription; 2],
}

struct HopReference {
    id: String,
    address: String,
    identities: String,
}

struct ProfileForm {
    id: String,
    initial_host: Option<String>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    user: Entity<InputState>,
    port: Entity<InputState>,
    group: Entity<InputState>,
    identities: Entity<InputState>,
    agent: Entity<InputState>,
    term: Entity<InputState>,
    connect_timeout: Entity<InputState>,
    keepalive: Entity<InputState>,
    set_env: Entity<TextareaState>,
    send_env: Entity<InputState>,
    proxy_command: Entity<InputState>,
    network_proxy: Entity<InputState>,
    proxy_command_mode: Cell<Option<bool>>,
    port_forwards: Entity<TextareaState>,
    identities_only: Cell<Option<bool>>,
    forward_agent: Cell<Option<bool>>,
    compression: Cell<Option<bool>>,
    hops: RefCell<Vec<HopForm>>,
    nav: Cell<usize>,
    tested: RefCell<Option<ConnectionProfile>>,
    _subscriptions: Vec<Subscription>,
}

impl ProfileForm {
    fn new(
        profile: Option<&ConnectionProfile>,
        saved: &[ConnectionProfile],
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let ssh = profile.map(|profile| &profile.ssh);
        let optional =
            |value: Option<u32>| value.map(|value| value.to_string()).unwrap_or_default();
        let env = ssh
            .map(|ssh| {
                ssh.set_env
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let set_env = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("LANG=zh_CN.UTF-8")
                .default_value(env)
        });
        let forwards = ssh
            .map(|ssh| {
                ssh.port_forwards
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let port_forwards = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("LocalForward 127.0.0.1:8080 localhost:80")
                .default_value(forwards)
        });
        let mut control = |placeholder, default| text_input(placeholder, default, window, cx);
        let field =
            |get: fn(&ConnectionProfile) -> &String| profile.map(get).cloned().unwrap_or_default();
        let mut form = Self {
            id: profile
                .filter(|p| !p.id.is_empty())
                .map(|p| p.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            initial_host: profile.map(|profile| profile.host.clone()),
            name: control("会话名称", field(|p| &p.name)),
            host: control("host.example.com", field(|p| &p.host)),
            user: control("SSH 用户", field(|p| &p.user)),
            port: control(
                "22",
                profile.map_or_else(|| "22".into(), |p| p.port.to_string()),
            ),
            group: control("默认分组", field(|p| &p.group)),
            identities: control(
                "~/.ssh/id_ed25519",
                profile.map_or_else(String::new, |p| p.identity_files.join("; ")),
            ),
            agent: control(
                "使用系统 SSH Agent",
                ssh.and_then(|ssh| ssh.identity_agent.clone())
                    .unwrap_or_default(),
            ),
            term: control(
                "xterm-256color",
                ssh.and_then(|ssh| ssh.term.clone()).unwrap_or_default(),
            ),
            connect_timeout: control(
                "使用全局设置",
                optional(ssh.and_then(|ssh| ssh.connect_timeout)),
            ),
            keepalive: control(
                "使用全局设置",
                optional(ssh.and_then(|ssh| ssh.keepalive_interval)),
            ),
            set_env,
            send_env: control(
                "例如 LANG LC_*",
                ssh.map(|ssh| ssh.send_env.join(" ")).unwrap_or_default(),
            ),
            proxy_command: control(
                "cloudflared access ssh --hostname %h",
                ssh.and_then(|ssh| ssh.proxy_command.as_deref())
                    .map(config::format_argv)
                    .unwrap_or_default(),
            ),
            network_proxy: control(
                "socks5://127.0.0.1:1080",
                ssh.and_then(|ssh| ssh.network_proxy.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
            proxy_command_mode: Cell::new(ssh.map(|ssh| ssh.proxy_command.is_some())),
            port_forwards,
            identities_only: Cell::new(ssh.map(|ssh| ssh.identities_only)),
            forward_agent: Cell::new(ssh.map(|ssh| ssh.forward_agent)),
            compression: Cell::new(ssh.map(|ssh| ssh.compression)),
            hops: RefCell::new(Vec::new()),
            nav: Cell::new(if profile.is_some_and(|p| !p.jumps.is_empty()) {
                2
            } else {
                0
            }),
            tested: RefCell::new(None),
            _subscriptions: Vec::new(),
        };
        if let Some(profile) = profile {
            *form.hops.get_mut() = profile
                .jumps
                .iter()
                .map(|hop| {
                    HopForm::new(
                        hop.reference
                            .as_ref()
                            .and_then(|id| saved.iter().find(|p| &p.id == id))
                            .map(|p| p.name.clone()),
                        config::ssh_address(&hop.user, &hop.host, Some(hop.port)),
                        hop.identity_files.join("; "),
                        hop.ssh.clone(),
                        hop.reference.clone(),
                        false,
                        window,
                        cx,
                    )
                })
                .collect();
        }
        form._subscriptions = [
            &form.name,
            &form.host,
            &form.user,
            &form.port,
            &form.group,
            &form.identities,
            &form.agent,
            &form.term,
            &form.connect_timeout,
            &form.keepalive,
            &form.send_env,
            &form.proxy_command,
            &form.network_proxy,
        ]
        .into_iter()
        .map(|input| refresh_on_change(input, window, cx))
        .collect();
        form
    }

    fn collect(&self, saved: &[ConnectionProfile], cx: &App) -> anyhow::Result<ConnectionProfile> {
        let local_user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let host = value(&self.host, cx);
        ensure!(!host.is_empty(), "请输入目标主机或 SSH 配置别名。");
        let (host, host_user, host_port) = config::parse_hop(&host)?;
        let alias = saved.iter().find(|profile| {
            self.initial_host.as_deref() != Some(host)
                && profile.source.is_some()
                && profile.name.eq_ignore_ascii_case(host)
        });
        let user = value(&self.user, cx);
        let user = if user.is_empty() {
            host_user
                .or_else(|| alias.map(|alias| alias.user.as_str()))
                .unwrap_or(&local_user)
        } else {
            &user
        };
        ensure!(
            !user.is_empty()
                && !user.contains(['/', '@', ':'])
                && !user.contains(char::is_whitespace),
            "请输入有效的 SSH 用户名。"
        );
        let port = value(&self.port, cx)
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| anyhow!("端口必须在 1–65535 之间。"))?;
        let port = host_port
            .or_else(|| alias.filter(|_| port == 22).map(|p| p.port))
            .unwrap_or(port);
        let name = value(&self.name, cx);
        let identities = value(&self.identities, cx);
        let hops = self.hops.borrow();
        let jumps = if hops.is_empty() {
            alias.map_or_else(Vec::new, |p| p.jumps.clone())
        } else {
            hops.iter()
                .enumerate()
                .map(|(index, hop)| {
                    let address = value(&hop.address, cx);
                    let (host, user, port) = config::parse_hop(&address)
                        .map_err(|error| anyhow!("第 {} 跳地址无效：{error}", index + 1))?;
                    let unqualified = user.is_none() && port.is_none();
                    let alias = saved.iter().find(|p| {
                        p.name.eq_ignore_ascii_case(host) && (unqualified || host != p.host)
                    });
                    let host = alias.map_or(host, |p| &p.host);
                    let user = user
                        .or_else(|| alias.map(|p| p.user.as_str()))
                        .unwrap_or(&local_user);
                    ensure!(!user.is_empty(), "请输入第 {} 跳的 SSH 用户。", index + 1);
                    let keys = value(&hop.identities, cx);
                    let reference = hop
                        .reference
                        .as_ref()
                        .filter(|reference| {
                            reference.address == address && reference.identities == keys
                        })
                        .map(|reference| reference.id.clone())
                        .or_else(|| {
                            alias
                                .filter(|_| unqualified && keys.is_empty())
                                .map(|alias| alias.id.clone())
                        });
                    Ok(Hop {
                        host: host.to_owned(),
                        user: user.to_owned(),
                        port: port.or_else(|| alias.map(|p| p.port)).unwrap_or(22),
                        identity_files: if keys.is_empty() {
                            alias.map_or_else(Vec::new, |p| p.identity_files.clone())
                        } else {
                            identity_files(&keys)
                        },
                        ssh: alias.map_or_else(|| hop.ssh.clone(), |p| p.ssh.clone()),
                        reference,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };
        let host = alias.map_or(host, |p| &p.host);
        let mut endpoints = HashSet::from([(host.to_ascii_lowercase(), port, user)]);
        ensure!(
            jumps.iter().all(|hop| endpoints.insert((
                hop.host.to_ascii_lowercase(),
                hop.port,
                &hop.user
            ))),
            "跳板链路中有重复的主机。"
        );
        let ssh = self.ssh_options(alias.map(|profile| &profile.ssh), cx)?;
        ensure!(
            jumps.is_empty() || ssh.proxy_command.is_none(),
            "ProxyCommand 和 ProxyJump 不能同时用于目标会话。"
        );
        Ok(ConnectionProfile {
            id: self.id.clone(),
            name: if name.is_empty() {
                host.to_owned()
            } else {
                name
            },
            host: host.to_owned(),
            user: user.to_owned(),
            port,
            identity_files: if identities.is_empty() {
                alias.map_or_else(Vec::new, |p| p.identity_files.clone())
            } else {
                identity_files(&identities)
            },
            jumps,
            source: None,
            group: value(&self.group, cx),
            ssh,
        })
    }

    fn ssh_options(&self, previous: Option<&SshOptions>, cx: &App) -> anyhow::Result<SshOptions> {
        let mut ssh = previous.cloned().unwrap_or_default();
        let agent = value(&self.agent, cx);
        let term = value(&self.term, cx);
        let timeout = value(&self.connect_timeout, cx);
        let keepalive = value(&self.keepalive, cx);
        let env = self.set_env.read(cx).value();
        let send = value(&self.send_env, cx);
        if !agent.is_empty() {
            ssh.identity_agent = Some(agent);
        }
        if !term.is_empty() {
            ensure!(
                !term.chars().any(|ch| ch.is_whitespace() || ch.is_control()),
                "终端类型不能包含空格或控制字符。"
            );
            ssh.term = Some(term);
        }
        if !timeout.is_empty() {
            let timeout = config::parse_seconds(&timeout)?;
            ensure!(timeout != 0, "连接超时须大于 0 秒。");
            ssh.connect_timeout = Some(timeout);
        }
        if !keepalive.is_empty() {
            ssh.keepalive_interval = Some(config::parse_seconds(&keepalive)?);
        }
        if !env.trim().is_empty() {
            for line in env.lines().filter(|line| !line.trim().is_empty()) {
                let (name, value) = config::parse_environment(line)?;
                ssh.set_env.insert(name.to_owned(), value.to_owned());
            }
        }
        if !send.is_empty() {
            ssh.send_env = send.split_whitespace().map(str::to_owned).collect();
        }
        if let Some(value) = self.identities_only.get() {
            ssh.identities_only = value;
        }
        if let Some(value) = self.forward_agent.get() {
            ssh.forward_agent = value;
        }
        if let Some(value) = self.compression.get() {
            ssh.compression = value;
        }
        if let Some(selected) = self.proxy_command_mode.get() {
            ssh.proxy_command = if selected {
                let command = config::parse_proxy_command(&value(&self.proxy_command, cx))?;
                ensure!(command.is_some(), "请输入 ProxyCommand。");
                command
            } else {
                None
            };
        }
        if ssh.proxy_command.is_some() {
            ssh.network_proxy = None;
        } else {
            let proxy = value(&self.network_proxy, cx);
            if !proxy.is_empty() {
                ssh.network_proxy = Some(config::NetworkProxy::parse(&proxy)?);
            }
        }
        let forwards = self.port_forwards.read(cx).value();
        if !forwards.trim().is_empty() {
            ssh.port_forwards = forwards
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(crate::ssh::PortForward::parse_line)
                .collect::<anyhow::Result<_>>()?;
        }
        Ok(ssh)
    }
}

impl HopForm {
    fn new(
        name: Option<String>,
        endpoint: String,
        keys: String,
        ssh: SshOptions,
        reference: Option<String>,
        editing: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let reference = reference.map(|id| HopReference {
            id,
            address: endpoint.clone(),
            identities: keys.clone(),
        });
        let address = text_input("user@host:port", endpoint, window, cx);
        let identities = text_input("~/.ssh/id_ed25519", keys, window, cx);
        let subscriptions =
            [&address, &identities].map(|input| refresh_on_change(input, window, cx));
        Self {
            name,
            address,
            identities,
            ssh,
            reference,
            editing: Cell::new(editing),
            _subscriptions: subscriptions,
        }
    }
}

fn text_input(
    label: &'static str,
    default: String,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(label)
            .default_value(default)
    })
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_owned()
}

fn identity_files(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

struct HopDrag {
    form: Rc<ProfileForm>,
    index: usize,
}

struct HopGhost(SharedString);

impl Render for HopGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p(px(8.))
            .rounded(px(6.))
            .bg(Theme::global(cx).popover)
            .text_color(Theme::global(cx).popover_foreground)
            .shadow_lg()
            .child(self.0.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Station {
    Origin,
    Jump,
    Gap,
    Target,
}

fn route_track(station: Station, palette: Palette) -> Div {
    let line = div()
        .absolute()
        .left(px(13.))
        .w(px(2.))
        .bg(palette.route)
        .top(if station == Station::Origin {
            px(14.)
        } else {
            px(0.)
        });
    let node_color = if station == Station::Origin {
        palette.dim
    } else {
        palette.route
    };
    div()
        .relative()
        .w(px(29.))
        .flex_shrink_0()
        .child(if station == Station::Target {
            line.h(px(16.))
        } else {
            line.bottom(px(0.))
        })
        .when(station != Station::Gap, |track| {
            track.child(
                row()
                    .absolute()
                    .left(px(7.))
                    .top(px(9.))
                    .size(px(14.))
                    .rounded_full()
                    .justify_center()
                    .border_2()
                    .border_color(node_color)
                    .bg(if station == Station::Jump {
                        palette.route
                    } else {
                        palette.raised
                    })
                    .when(station == Station::Target, |node| {
                        node.child(div().size(px(6.)).rounded_full().bg(palette.route))
                    }),
            )
        })
}

fn route_body(
    form: &Rc<ProfileForm>,
    saved: &[ConnectionProfile],
    view: &WeakEntity<Workbench>,
    palette: Palette,
    cx: &App,
) -> Div {
    let mono = Theme::global(cx).mono_font_family.clone();
    let valid = form.collect(saved, cx).ok();
    let tested = valid.is_some() && valid.as_ref() == form.tested.borrow().as_ref();
    let workbench = view.upgrade();
    let progress = |index| {
        use crate::ssh::HopStage;
        let result = tested
            .then(|| workbench.as_ref()?.read(cx).test_progress(&form.id, index))
            .flatten()?;
        let (color, label) = match result.stage {
            HopStage::Connected => (
                palette.green,
                result
                    .rtt_ms
                    .map_or_else(|| "已验证".into(), |ms| format!("已验证，{ms} ms")),
            ),
            HopStage::Failed => (palette.danger, "连接失败".into()),
            HopStage::Trust => (palette.warn, "等待确认".into()),
            HopStage::ProxyApproval => (palette.warn, "等待确认代理命令".into()),
            HopStage::Authenticating => (palette.warn, "正在认证".into()),
            HopStage::Connecting => (palette.warn, "正在连接".into()),
        };
        Some(
            row()
                .gap(px(4.))
                .text_size(px(12.))
                .text_color(color)
                .child(icon(
                    if result.stage == HopStage::Connected {
                        IconName::CircleCheck
                    } else {
                        IconName::Circle
                    },
                    13.,
                    color,
                ))
                .child(label),
        )
    };
    let machine = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let mut route = col().mt(px(5.)).child(
        row()
            .items_stretch()
            .child(route_track(Station::Origin, palette))
            .child(
                row()
                    .gap(px(8.))
                    .h(px(34.))
                    .child(icon(IconName::Laptop, 15., palette.dim))
                    .child("本机")
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_color(palette.dim)
                            .child(machine),
                    ),
            ),
    );
    let hops = form.hops.borrow();
    for (index, hop) in hops.iter().enumerate() {
        let reorder = form.clone();
        let remove = form.clone();
        let edit = form.clone();
        let editing = hop.editing.get();
        let address = value(&hop.address, cx);
        let identity = value(&hop.identities, cx);
        let subtitle = if identity.is_empty() {
            "⚿  SSH Agent / 默认私钥".into()
        } else {
            format!("⚿  {identity}")
        };
        let drag_label: SharedString = format!("第 {} 跳", index + 1).into();
        route = route.child(
            row()
                .items_stretch()
                .child(route_track(Station::Jump, palette))
                .child(
                    col()
                        .flex_1()
                        .mb(px(9.))
                        .gap(px(6.))
                        .rounded(px(7.))
                        .bg(palette.panel)
                        .border_1()
                        .border_color(palette.line)
                        .p(px(9.))
                        .child(
                            row()
                                .gap(px(7.))
                                .child(
                                    div()
                                        .id(("jump-grip", index))
                                        .cursor_grab()
                                        .text_color(palette.dim)
                                        .child(icon(IconName::GripVertical, 15., palette.dim))
                                        .on_drag(
                                            HopDrag {
                                                form: form.clone(),
                                                index,
                                            },
                                            move |_, _, _, cx| {
                                                cx.new(|_| HopGhost(drag_label.clone()))
                                            },
                                        ),
                                )
                                .child(
                                    div()
                                        .text_color(palette.route)
                                        .child(format!("第 {} 跳", index + 1)),
                                )
                                .children(hop.name.as_ref().map(|name| {
                                    div().font_weight(FontWeight::SEMIBOLD).child(name.clone())
                                }))
                                .when(hop.reference.is_some(), |heading| {
                                    heading.child(
                                        div()
                                            .text_size(px(11.))
                                            .px(px(5.))
                                            .border_1()
                                            .border_color(palette.line)
                                            .rounded(px(4.))
                                            .text_color(palette.muted)
                                            .child("已保存会话"),
                                    )
                                })
                                .child(div().flex_1())
                                .child(
                                    tool(
                                        ("edit-hop", index),
                                        if editing { "✓" } else { "⋯" },
                                        "编辑跳板",
                                        palette,
                                    )
                                    .on_click(
                                        move |_, window, _| {
                                            if let Some(hop) = edit.hops.borrow().get(index) {
                                                hop.editing.set(!hop.editing.get());
                                            }
                                            window.refresh();
                                        },
                                    ),
                                )
                                .child(
                                    tool(("remove-hop", index), "×", "移除跳板", palette).on_click(
                                        move |_, window, _| {
                                            remove.hops.borrow_mut().remove(index);
                                            window.refresh();
                                        },
                                    ),
                                ),
                        )
                        .when(editing, |card| {
                            card.child(
                                input(&hop.address, "跳板地址", palette).font_family(mono.clone()),
                            )
                            .child(
                                input(&hop.identities, "跳板私钥路径", palette)
                                    .font_family(mono.clone()),
                            )
                        })
                        .when(!editing, |card| {
                            card.child(
                                div()
                                    .font_family(mono.clone())
                                    .text_color(palette.muted)
                                    .child(if address.is_empty() {
                                        "尚未填写地址".into()
                                    } else {
                                        address
                                    }),
                            )
                            .child(
                                row()
                                    .gap(px(8.))
                                    .child(description_text(subtitle, palette).truncate())
                                    .children(progress(index)),
                            )
                        }),
                )
                .id(("jump-card", index))
                .on_drop(move |drag: &HopDrag, window, _| {
                    if Rc::ptr_eq(&reorder, &drag.form) && drag.index != index {
                        let mut hops = reorder.hops.borrow_mut();
                        if drag.index < hops.len() && index < hops.len() {
                            let moved = hops.remove(drag.index);
                            hops.insert(index, moved);
                            window.refresh();
                        }
                    }
                }),
        );
    }
    let add = form.clone();
    let menu_form = form.clone();
    let menu_profiles = saved
        .iter()
        .filter(|p| p.id != form.id)
        .cloned()
        .collect::<Vec<_>>();
    let proxy_mode = form.proxy_command_mode.get().unwrap_or(false);
    let add_enabled = hops.len() < 8 && !proxy_mode;
    let preview = valid
        .as_ref()
        .map(ConnectionProfile::ssh_command)
        .filter(|command| !command.is_empty());
    let copy = preview.clone();
    let reuse = view
        .upgrade()
        .is_some_and(|view| view.read(cx).settings.reuse_jumps);
    let mode = |id, title, detail, command_mode| {
        let set = form.clone();
        let active = proxy_mode == command_mode;
        button(id, "", palette)
            .h_auto()
            .flex_1()
            .border_color(if active { palette.accent } else { palette.line })
            .when(active, |button| button.bg(palette.accent.opacity(0.08)))
            .child(
                col()
                    .w_full()
                    .gap(px(3.))
                    .p(px(5.))
                    .child(
                        div()
                            .text_color(if active { palette.accent } else { palette.ink })
                            .child(title),
                    )
                    .child(description_text(detail, palette)),
            )
            .on_click(move |_, window, _| {
                set.proxy_command_mode.set(Some(command_mode));
                window.refresh();
            })
    };
    section(
        "跳板链路",
        "按顺序经过下列跳板机到达目标主机。可直接填写地址，或沿用已保存会话的认证设置。",
        palette,
    )
    .child(
        route
            .child(
                row()
                    .items_stretch()
                    .child(route_track(Station::Gap, palette))
                    .child(
                        row()
                            .flex_1()
                            .gap(px(8.))
                            .pb(px(8.))
                            .child(
                                DropdownButton::new("add-hop")
                                    .secondary()
                                    .outline()
                                    .with_size(px(32.))
                                    .menu_accessibility_label("选用已保存会话作为跳板")
                                    .button(
                                        Button::new("add-manual-hop")
                                            .label("＋  在目标前添加跳板")
                                            .disabled(!add_enabled)
                                            .on_click(move |_, window, cx| {
                                                add_hop(&add, None, window, cx)
                                            }),
                                    )
                                    .dropdown_menu(move |menu, _, _| {
                                        let mut menu =
                                            menu.item(PopupMenuItem::label("选择已保存会话"));
                                        for profile in &menu_profiles {
                                            let profile = profile.clone();
                                            let form = menu_form.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(profile.name.clone())
                                                    .disabled(!add_enabled)
                                                    .on_click(move |_, window, cx| {
                                                        add_hop(&form, Some(&profile), window, cx)
                                                    }),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .child(
                                description_text("可选已保存会话，或输入 user@host:port", palette)
                                    .truncate(),
                            ),
                    ),
            )
            .child(
                row()
                    .items_stretch()
                    .child(route_track(Station::Target, palette))
                    .child(
                        col()
                            .flex_1()
                            .gap(px(5.))
                            .p(px(11.))
                            .border_1()
                            .border_color(palette.route.opacity(0.55))
                            .bg(palette.route.opacity(0.06))
                            .rounded(px(6.))
                            .child(
                                row()
                                    .gap(px(7.))
                                    .child(div().text_color(palette.route).child("目标"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(value(&form.name, cx)),
                                    ),
                            )
                            .child(div().font_family(mono.clone()).child(
                                form.collect(saved, cx).map_or_else(
                                    |_| "请先填写基本信息".into(),
                                    |profile| target(&profile),
                                ),
                            ))
                            .children(progress(hops.len())),
                    ),
            ),
    )
    .child(div().h(px(1.)).mt(px(5.)).bg(palette.line))
    .child(div().font_weight(FontWeight::SEMIBOLD).child("连接方式"))
    .child(
        row()
            .items_stretch()
            .gap(px(9.))
            .child(mode(
                "route-proxy-jump",
                "ProxyJump（推荐）",
                "逐跳建立 SSH 隧道，各跳独立认证。",
                false,
            ))
            .child(mode(
                "route-proxy-command",
                "ProxyCommand",
                "运行本地程序，通过标准输入输出建立连接。",
                true,
            )),
    )
    .when(proxy_mode, |body| {
        body.child(input(&form.proxy_command, "ProxyCommand", palette).font_family(mono.clone()))
            .child(description_text(
                "支持 %h、%p、%r、%n、%%。连接时需确认运行命令。",
                palette,
            ))
    })
    .when(!proxy_mode, |body| {
        body.child(field(
            "网络代理",
            input(&form.network_proxy, "网络代理地址", palette).font_family(mono.clone()),
            palette,
        ))
        .child(description_text(
            "在第一跳之前连接 SOCKS5 或 HTTP CONNECT 代理，留空使用直连。",
            palette,
        ))
    })
    .child(
        row()
            .justify_between()
            .gap(px(10.))
            .py(px(4.))
            .child(
                col()
                    .gap(px(3.))
                    .child("复用跳板连接")
                    .child(description_text(
                        "多个会话可共用已验证且已认证的跳板连接。",
                        palette,
                    )),
            )
            .child(super::settings::toggle_setting(
                "profile-reuse-jumps",
                reuse,
                view,
                |s, value| s.reuse_jumps = value,
            )),
    )
    .child(div().h(px(1.)).mt(px(5.)).bg(palette.line))
    .child(
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .child("等效 OpenSSH 命令"),
    )
    .child(
        row()
            .gap(px(6.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(palette.line)
            .bg(palette.base)
            .child(
                div()
                    .font_family(mono)
                    .flex_1()
                    .truncate()
                    .child(preview.unwrap_or_else(|| {
                        if valid.is_some() {
                            "此航线使用 CapTerm 的代理配置".into()
                        } else {
                            "完成目标地址后生成命令".into()
                        }
                    })),
            )
            .child(
                tool("copy-ssh-command", "▣", "复制 SSH 命令", palette)
                    .disabled(copy.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(command) = &copy {
                            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                        }
                    }),
            ),
    )
}

fn add_hop(
    form: &ProfileForm,
    profile: Option<&ConnectionProfile>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut hops = form.hops.borrow_mut();
    if hops.len() >= 8 {
        return;
    }
    hops.push(HopForm::new(
        profile.map(|p| p.name.clone()),
        profile.map_or_else(String::new, |p| {
            config::ssh_address(&p.user, &p.host, Some(p.port))
        }),
        profile.map_or_else(String::new, |p| p.identity_files.join("; ")),
        profile.map(|p| p.ssh.clone()).unwrap_or_default(),
        profile.map(|p| p.id.clone()),
        profile.is_none(),
        window,
        cx,
    ));
    window.refresh();
}

impl Workbench {
    pub(in crate::workbench) fn move_profile_group(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = text_input("默认分组", profile.group.clone(), window, cx);
        let groups = self
            .settings
            .profiles
            .iter()
            .filter(|item| item.source.is_none())
            .map(|item| item.group.clone())
            .chain([String::new()])
            .collect::<std::collections::BTreeSet<_>>();
        let view = cx.weak_entity();
        let status = Rc::new(FormStatus::default());
        let palette = Palette::new(self.settings.light);
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let group = group.clone();
            let group_menu = group.clone();
            let groups = groups.clone();
            let id = profile.id.clone();
            let save_status = status.clone();
            let choices = DropdownButton::new("existing-profile-groups")
                .secondary()
                .outline()
                .with_size(px(33.))
                .button(Button::new("choose-existing-group").label("已有分组"))
                .dropdown_menu(move |menu, _, _| {
                    groups.iter().fold(menu, |menu, name| {
                        let group = group_menu.clone();
                        let name = name.clone();
                        menu.item(
                            PopupMenuItem::new(if name.is_empty() {
                                "默认分组".to_owned()
                            } else {
                                name.clone()
                            })
                            .on_click(move |_, window, cx| {
                                group.update(cx, |group, cx| {
                                    group.set_value(name.clone(), window, cx)
                                })
                            }),
                        )
                    })
                });
            let form = row()
                .gap(px(8.))
                .child(div().flex_1().child(input(&group, "分组名称", palette)))
                .child(choices);
            styled_dialog(dialog, 480., palette).child(
                col()
                    .child(detailed_head(
                        "移动到分组",
                        format!("为「{}」选择或创建分组。", profile.name),
                        palette,
                        false,
                    ))
                    .child(
                        col()
                            .p(px(20.))
                            .gap(px(8.))
                            .child(form)
                            .children(status.error(palette)),
                    )
                    .child(dialog_actions(palette).child(cancel(palette, false)).child(
                        primary("save-profile-group", "移动", palette).on_click(
                            move |_, window, cx| {
                                let group = value(&group, cx);
                                let _ = view.update(cx, |this, cx| {
                                    let mut settings = this.settings.clone();
                                    let Some(profile) = settings
                                        .profiles
                                        .iter_mut()
                                        .find(|item| item.id == id && item.source.is_none())
                                    else {
                                        save_status
                                            .fail("该会话已移除或改由 SSH 配置文件管理。", window);
                                        return;
                                    };
                                    profile.group = group;
                                    if this.persist(settings, cx) {
                                        window.close_dialog(cx);
                                    } else {
                                        save_status.fail(this.notice.clone(), window);
                                    }
                                });
                            },
                        ),
                    )),
            )
        });
    }

    pub(in crate::workbench) fn profile_dialog(
        &mut self,
        profile: Option<ConnectionProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(profile) = profile.as_ref().filter(|p| p.source.is_some()) {
            self.linked_profile_dialog(profile.clone(), window, cx);
            return;
        }
        self.edit_profile_dialog(profile, None, window, cx);
    }

    fn edit_profile_dialog(
        &mut self,
        mut profile: Option<ConnectionProfile>,
        convert_from: Option<ConnectionProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(profile) = &mut profile {
            self.settings.resolve_jump_references(profile);
        }
        let form = Rc::new(ProfileForm::new(
            profile.as_ref(),
            &self.settings.profiles,
            window,
            cx,
        ));
        let saved = self.settings.profiles.clone();
        let status = Rc::new(FormStatus::default());
        let palette = Palette::new(self.settings.light);
        let view = cx.weak_entity();
        let title = profile
            .as_ref()
            .map_or_else(|| "新建会话".into(), |p| format!("编辑会话 {}", p.name));
        let subtitle = profile.as_ref().map_or_else(
            || "设置 SSH 目标、认证方式和跳板链路。".into(),
            |p| {
                format!(
                    "保存在「{}」分组",
                    if p.group.is_empty() {
                        "默认分组"
                    } else {
                        &p.group
                    }
                )
            },
        );
        window.open_dialog(cx, move |dialog, window, cx| {
            let nav_width = if window.viewport_size().width < px(640.) {
                124.
            } else {
                184.
            };
            let nav = navigation(nav_width, palette).children(NAV.into_iter().enumerate().map(
                |(index, (symbol, label))| {
                    let form = form.clone();
                    nav_button(index, symbol, label, form.nav.get() == index, palette).on_click(
                        move |_, window, _| {
                            form.nav.set(index);
                            window.refresh();
                        },
                    )
                },
            ));
            let mono = Theme::global(cx).mono_font_family.clone();
            let body = match form.nav.get() {
                0 => section(
                    "基本信息",
                    "名称与分组用于组织会话。可以输入主机名、IP 地址或已导入的 SSH 别名。",
                    palette,
                )
                .child(field(
                    "会话名称",
                    input(&form.name, "会话名称", palette),
                    palette,
                ))
                .child(field("分组", input(&form.group, "分组", palette), palette))
                .child(field(
                    "主机",
                    input(&form.host, "主机", palette).font_family(mono.clone()),
                    palette,
                ))
                .child(
                    row()
                        .gap(px(10.))
                        .child(
                            field(
                                "用户",
                                input(&form.user, "用户", palette).font_family(mono.clone()),
                                palette,
                            )
                            .flex_1(),
                        )
                        .child(
                            field(
                                "端口",
                                input(&form.port, "端口", palette).font_family(mono),
                                palette,
                            )
                            .w(px(105.)),
                        ),
                ),
                1 => {
                    let options = form.clone();
                    let forwarding = form.clone();
                    section(
                        "身份验证",
                        "使用私钥和 SSH Agent，也可以从会话菜单使用密码连接。",
                        palette,
                    )
                    .child(field(
                        "私钥路径",
                        input(&form.identities, "私钥路径", palette).font_family(mono.clone()),
                        palette,
                    ))
                    .child(description_text(
                        "使用分号分隔多个文件，留空时使用默认 SSH 私钥。",
                        palette,
                    ))
                    .child(field(
                        "SSH Agent",
                        input(&form.agent, "SSH Agent", palette).font_family(mono),
                        palette,
                    ))
                    .child(description_text(
                        "留空使用系统 Agent，输入 none 禁用，也可填写本机套接字路径。",
                        palette,
                    ))
                    .child(
                        row()
                            .justify_between()
                            .gap(px(10.))
                            .child("仅尝试指定的密钥")
                            .child(
                                Switch::new("identities-only")
                                    .checked(form.identities_only.get().unwrap_or(false))
                                    .on_click(move |checked, window, _| {
                                        options.identities_only.set(Some(*checked));
                                        window.refresh();
                                    }),
                            ),
                    )
                    .child(
                        row()
                            .justify_between()
                            .gap(px(10.))
                            .child(col().gap(px(4.)).child("转发 SSH Agent").child(
                                description_text(
                                    "允许远端通过当前 SSH 连接使用本机 Agent 签名。",
                                    palette,
                                ),
                            ))
                            .child(
                                Switch::new("forward-ssh-agent")
                                    .checked(form.forward_agent.get().unwrap_or(false))
                                    .on_click(move |checked, window, _| {
                                        forwarding.forward_agent.set(Some(*checked));
                                        window.refresh();
                                    }),
                            ),
                    )
                }
                2 => route_body(&form, &saved, &view, palette, cx),
                3 => section("终端", "终端字体、行号与时间戳使用全局外观设置。", palette)
                    .child(field(
                        "终端类型 TERM",
                        input(&form.term, "终端类型 TERM", palette).font_family(mono.clone()),
                        palette,
                    ))
                    .child(description_text(
                        "留空使用 xterm-256color。会同时尝试发送 COLORTERM=truecolor。",
                        palette,
                    ))
                    .child(field(
                        "发送本机环境变量（SendEnv）",
                        input(&form.send_env, "发送本机环境变量", palette)
                            .font_family(mono.clone()),
                        palette,
                    ))
                    .child(description_text(
                        "使用空格分隔名称或通配模式，远端须通过 AcceptEnv 放行。",
                        palette,
                    ))
                    .child(field(
                        "设置远端环境变量（SetEnv）",
                        Textarea::new(&form.set_env)
                            .aria_label("远端环境变量")
                            .w_full()
                            .h(px(105.))
                            .font_family(mono)
                            .bg(palette.input)
                            .border_color(palette.line)
                            .rounded(px(6.)),
                        palette,
                    ))
                    .child(description_text("每行使用 NAME=value 格式。", palette)),
                4 => section(
                    "端口转发",
                    "每行添加一条 OpenSSH 规则。连接建立后按顺序启用。",
                    palette,
                )
                .child(
                    Textarea::new(&form.port_forwards)
                        .aria_label("端口转发规则")
                        .w_full()
                        .h(px(180.))
                        .font_family(mono)
                        .bg(palette.input)
                        .border_color(palette.line)
                        .rounded(px(6.)),
                )
                .child(description_text(
                    "LocalForward 127.0.0.1:8080 localhost:80",
                    palette,
                ))
                .child(description_text(
                    "RemoteForward 127.0.0.1:9080 localhost:80",
                    palette,
                ))
                .child(description_text("DynamicForward 127.0.0.1:1080", palette)),
                _ => {
                    let options = form.clone();
                    section(
                        "高级",
                        "每一跳均单独执行 SSH 握手、身份验证和主机密钥校验。",
                        palette,
                    )
                    .child(field(
                        "连接超时（秒）",
                        input(&form.connect_timeout, "连接超时", palette).font_family(mono.clone()),
                        palette,
                    ))
                    .child(field(
                        "保活间隔（秒）",
                        input(&form.keepalive, "保活间隔", palette).font_family(mono),
                        palette,
                    ))
                    .child(description_text(
                        "留空使用全局设置。保活间隔填 0 可以关闭 SSH 保活。",
                        palette,
                    ))
                    .child(
                        row()
                            .justify_between()
                            .gap(px(10.))
                            .child("压缩 SSH 数据")
                            .child(
                                Switch::new("session-compression")
                                    .checked(form.compression.get().unwrap_or(false))
                                    .on_click(move |checked, window, _| {
                                        options.compression.set(Some(*checked));
                                        window.refresh();
                                    }),
                            ),
                    )
                }
            };
            let save = |connect| {
                let view = view.clone();
                let form = form.clone();
                let status = status.clone();
                let convert_from = convert_from.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = view.update(cx, |this, cx| {
                        match form.collect(&this.settings.profiles, cx) {
                            Ok(profile) => {
                                let mut settings = this.settings.clone();
                                if let Some(imported) = &convert_from {
                                    settings.profiles.retain(|item| item.id != imported.id);
                                    if let Some(source) =
                                        settings.linked_configs.iter_mut().find(|source| {
                                            imported.source.as_ref() == Some(&source.path)
                                        })
                                    {
                                        source.excluded.insert(imported.name.clone());
                                    }
                                }
                                if let Some(existing) = settings
                                    .profiles
                                    .iter_mut()
                                    .find(|item| item.id == profile.id)
                                {
                                    *existing = profile.clone();
                                } else {
                                    settings.profiles.push(profile.clone());
                                }
                                if this.persist(settings, cx) {
                                    this.announce(format!("已保存会话 · {}", profile.name), cx);
                                    window.close_dialog(cx);
                                    if connect {
                                        this.connect(profile, None, window, cx);
                                    }
                                } else {
                                    status.fail(this.notice.clone(), window);
                                }
                            }
                            Err(error) => status.fail(error.to_string(), window),
                        }
                    });
                }
            };
            let test_status = status.clone();
            let test_form = form.clone();
            let test_view = view.clone();
            let busy = status.busy.get();
            styled_dialog(dialog, 880., palette).child(
                col()
                    .h(px(780.).min(window.viewport_size().height - px(40.)))
                    .child(detailed_head(
                        title.clone(),
                        subtitle.clone(),
                        palette,
                        false,
                    ))
                    .child(
                        row().items_stretch().flex_1().min_h_0().child(nav).child(
                            col()
                                .id("profile-body")
                                .flex_1()
                                .overflow_y_scroll()
                                .child(body),
                        ),
                    )
                    .child(
                        dialog_actions(palette)
                            .child(
                                button(
                                    "test-profile",
                                    if busy {
                                        "正在测试…"
                                    } else {
                                        "⌁  测试连接"
                                    },
                                    palette,
                                )
                                .disabled(busy)
                                .on_click(move |_, window, cx| {
                                    let _ = test_view.update(cx, |this, cx| {
                                        match test_form.collect(&this.settings.profiles, cx) {
                                            Ok(profile) => {
                                                test_form.tested.replace(Some(profile.clone()));
                                                test_status.start(window);
                                                this.test_form(
                                                    profile,
                                                    test_status.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                            Err(error) => {
                                                test_status.fail(error.to_string(), window)
                                            }
                                        }
                                    });
                                }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .children(status.error(palette)),
                            )
                            .child(cancel(palette, false))
                            .child(
                                button("save-profile", "保存", palette)
                                    .disabled(busy)
                                    .on_click(save(false)),
                            )
                            .child(
                                primary("save-connect-profile", "保存并连接", palette)
                                    .disabled(busy)
                                    .on_click(save(true)),
                            ),
                    ),
            )
        });
    }

    fn linked_profile_dialog(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        let palette = Palette::new(self.settings.light);
        window.open_dialog(cx, move |dialog, _, _| {
            let source = profile
                .source
                .as_deref()
                .map(config::display_path)
                .unwrap_or_default();
            let convert = profile.clone();
            let view = view.clone();
            styled_dialog(dialog, 530., palette).child(
                col()
                    .child(detailed_head(
                        format!("查看会话 {}", profile.name),
                        "该会话由 SSH 配置文件管理。",
                        palette,
                        false,
                    ))
                    .child(
                        col()
                            .gap(px(9.))
                            .p(px(20.))
                            .child(target(&profile))
                            .child(description_text(source, palette)),
                    )
                    .child(dialog_actions(palette).child(cancel(palette, false)).child(
                        primary("convert-linked-profile", "转为本地会话", palette).on_click(
                            move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    let mut local = convert.clone();
                                    local.source = None;
                                    local.id = uuid::Uuid::new_v4().to_string();
                                    window.close_dialog(cx);
                                    this.edit_profile_dialog(
                                        Some(local),
                                        Some(convert.clone()),
                                        window,
                                        cx,
                                    );
                                });
                            },
                        ),
                    )),
            )
        });
    }
}
