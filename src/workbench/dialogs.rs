mod import;
mod profile;
mod settings;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::{InputContentType, InputEvent, InputState};
use gpui_kit::component::{Disableable, Theme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::Workbench;
use super::ui::{Palette, button, chrome_icon_tool, col, icon, input, primary, row, target, tool};
use crate::config::{AuthMethod, ConnectionProfile};
use crate::credentials;
use crate::ssh::{ProxyCommandRequest, TrustDecision, TrustRequest};

#[derive(Clone)]
pub(super) enum CredentialTarget {
    Connect,
    NewTab,
    Test(Option<Rc<FormStatus>>),
    Reconnect(u64),
}

#[derive(Default)]
pub(super) struct FormStatus {
    busy: Cell<bool>,
    message: RefCell<Option<(String, bool)>>,
}

impl FormStatus {
    pub(super) fn start(&self, window: &mut Window) {
        self.busy.set(true);
        self.message.borrow_mut().take();
        window.refresh();
    }

    pub(super) fn fail(&self, error: impl Into<String>, window: &mut Window) {
        self.finish(error.into(), false, window);
    }

    pub(super) fn finish(&self, message: String, ok: bool, window: &mut Window) {
        self.busy.set(false);
        self.message.replace(Some((message, ok)));
        window.refresh();
    }

    fn error(&self, palette: Palette) -> Option<Div> {
        self.message.borrow().as_ref().map(|(message, ok)| {
            div()
                .text_size(px(12.))
                .text_color(if *ok { palette.green } else { palette.warn })
                .child(message.clone())
        })
    }
}

fn styled_dialog(dialog: Dialog, width: f32, palette: Palette) -> Dialog {
    dialog
        .w(px(width))
        .centered()
        .p_0()
        .gap_0()
        .bg(if palette.base.l > 0.5 {
            rgb(0xffffff)
        } else {
            rgb(0x1b2129)
        })
        .border_color(palette.line)
        .rounded(px(10.))
        .shadow_lg()
        .close_button(false)
        .overlay_closable(false)
        .entrance(DialogEntrance::None)
        .text_color(palette.ink)
        .text_size(px(13.))
}

fn dialog_head(title: impl Into<SharedString>, palette: Palette, busy: bool) -> Div {
    detailed_head(title, "", palette, busy)
}

fn detailed_head(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    palette: Palette,
    busy: bool,
) -> Div {
    let description = description.into();
    row()
        .justify_between()
        .gap(px(10.))
        .px(px(20.))
        .py(px(14.))
        .border_b_1()
        .border_color(palette.line)
        .flex_shrink_0()
        .child(
            col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(17.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .when(!description.is_empty(), |head| {
                    head.child(description_text(description, palette))
                }),
        )
        .child(
            chrome_icon_tool("dialog-x", IconName::X, "关闭", palette)
                .disabled(busy)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
}

fn dialog_actions(palette: Palette) -> Div {
    row()
        .justify_end()
        .gap(px(8.))
        .px(px(20.))
        .py(px(12.))
        .flex_shrink_0()
        .border_t_1()
        .border_color(palette.line)
}

fn cancel(palette: Palette, busy: bool) -> gpui_kit::component::button::Button {
    button("dialog-cancel", "取消", palette)
        .disabled(busy)
        .on_click(|_, window, cx| window.close_dialog(cx))
}

fn field(label: &'static str, control: impl IntoElement, palette: Palette) -> Div {
    col()
        .gap(px(4.))
        .child(div().text_color(palette.muted).child(label))
        .child(control)
}

fn description_text(text: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .text_color(palette.muted)
        .text_size(px(12.))
        .line_height(px(19.))
        .child(text.into())
}

fn section(title: &'static str, description: &'static str, palette: Palette) -> Div {
    col()
        .flex_shrink_0()
        .gap(px(9.))
        .px(px(20.))
        .py(px(15.))
        .border_b_1()
        .border_color(palette.line)
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when(!description.is_empty(), |section| {
            section.child(description_text(description, palette))
        })
}

fn navigation(width: f32, palette: Palette) -> Div {
    col()
        .w(px(width))
        .flex_shrink_0()
        .gap(px(3.))
        .p(px(10.))
        .border_r_1()
        .border_color(palette.line)
        .bg(palette.panel)
}

fn nav_button(
    id: usize,
    symbol: IconName,
    label: &'static str,
    active: bool,
    palette: Palette,
) -> gpui_kit::component::button::Button {
    button(("dialog-nav", id), label, palette)
        .icon(icon(
            symbol,
            16.,
            if active {
                palette.accent
            } else {
                palette.muted
            },
        ))
        .h(px(36.))
        .flex_shrink_0()
        .px(px(10.))
        .border_0()
        .justify_start()
        .text_color(palette.muted)
        .when(active, |nav| {
            nav.text_color(palette.accent)
                .bg(palette.accent.opacity(0.14))
        })
}

fn refresh_on_change(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) -> Subscription {
    window.subscribe(input, cx, |_, event: &InputEvent, window, _| {
        if matches!(event, InputEvent::Change) {
            window.refresh();
        }
    })
}

impl Workbench {
    pub(super) fn password_dialog(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.credential_dialog(profile, CredentialTarget::Connect, None, window, cx);
    }

    pub(super) fn test_with_password(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.credential_dialog(profile, CredentialTarget::Test(None), None, window, cx);
    }

    pub(super) fn load_or_prompt_password(
        &mut self,
        profile: &ConnectionProfile,
        action: CredentialTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let result = if self.can_remember_password(profile) {
            credentials::load(profile)
        } else {
            Ok(None)
        };
        match result {
            Ok(Some(password)) => Some(password),
            result => {
                let error = result.err().map(|error| format!("{error:#}"));
                self.credential_dialog(profile.clone(), action, error, window, cx);
                None
            }
        }
    }

    fn can_remember_password(&self, profile: &ConnectionProfile) -> bool {
        profile.source.is_none()
            && self
                .settings
                .profiles
                .iter()
                .any(|saved| credentials::same_target(saved, profile))
    }

    pub(super) fn credential_dialog(
        &mut self,
        profile: ConnectionProfile,
        action: CredentialTarget,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("SSH 密码")
        });
        let view = cx.weak_entity();
        let status = Rc::new(FormStatus::default());
        if let Some(error) = error {
            status.fail(error, window);
        }
        let can_remember = profile.auth == AuthMethod::Password
            && self.can_remember_password(&profile)
            && !matches!(&action, CredentialTarget::Test(Some(_)));
        let remember = Rc::new(Cell::new(false));
        let focus_password = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let password_focus = password.focus_handle(cx);
            if focus_password.replace(false) {
                password_focus.focus(window, cx);
            }
            let (description, label) = match &action {
                CredentialTarget::Connect | CredentialTarget::NewTab => ("连接到", "连接"),
                CredentialTarget::Test(_) => ("测试连接到", "测试连接"),
                CredentialTarget::Reconnect(_) => ("重新连接到", "重新连接"),
            };
            let body = col()
                .gap(px(12.))
                .p(px(15.))
                .child(
                    div()
                        .text_color(palette.muted)
                        .child(format!("{description} {}", target(&profile))),
                )
                .child(field(
                    "密码",
                    input(&password, "SSH 密码", palette).content_type(InputContentType::Password),
                    palette,
                ))
                .when(can_remember, |body| {
                    let remember = remember.clone();
                    body.child(
                        Checkbox::new("remember-ssh-password")
                            .label("记住密码")
                            .checked(remember.get())
                            .on_click(move |checked, window, _| {
                                remember.set(*checked);
                                window.refresh();
                            }),
                    )
                    .child(description_text("密码保存在系统凭据库中。", palette))
                })
                .children(status.error(palette));
            let view = view.clone();
            let password = password.clone();
            let profile = profile.clone();
            let status = status.clone();
            let action = action.clone();
            let remember = remember.clone();
            let submit = move |window: &mut Window, cx: &mut App| {
                let credential = password.read(cx).value().to_string();
                if credential.is_empty() {
                    status.fail("请输入 SSH 密码。", window);
                    return;
                }
                let _ = view.update(cx, |this, cx| {
                    if remember.get()
                        && this.can_remember_password(&profile)
                        && let Err(error) = credentials::save(&profile, &credential)
                    {
                        status.fail(format!("{error:#}"), window);
                        return;
                    }
                    password.update(cx, |password, cx| password.clean(window, cx));
                    window.close_dialog(cx);
                    let credential = Some(credential);
                    match &action {
                        CredentialTarget::Connect => {
                            this.connect(profile.clone(), credential, window, cx)
                        }
                        CredentialTarget::NewTab => {
                            this.connect_new(profile.clone(), credential, window, cx)
                        }
                        CredentialTarget::Test(form) => {
                            this.request_test(profile.clone(), credential, form.clone(), window, cx)
                        }
                        CredentialTarget::Reconnect(id) => {
                            this.reconnect_ssh(*id, credential, window, cx)
                        }
                    }
                });
            };
            let click_submit = submit.clone();
            styled_dialog(dialog, 470., palette).child(
                col()
                    .capture_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "enter"
                            && !event.keystroke.modifiers.modified()
                            && password_focus.is_focused(window)
                        {
                            cx.stop_propagation();
                            submit(window, cx);
                        }
                    })
                    .child(dialog_head("SSH 密码", palette, false))
                    .child(body)
                    .child(
                        dialog_actions(palette).child(cancel(palette, false)).child(
                            primary("connect-password", label, palette)
                                .on_click(move |_, window, cx| click_submit(window, cx)),
                        ),
                    ),
            )
        });
    }

    pub(super) fn remove_profile(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let view = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let profile = profile.clone();
            let name = profile.name.clone();
            let view = view.clone();
            let delete = primary("delete-saved-profile", "删除会话", palette).on_click(
                move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        let mut settings = this.settings.clone();
                        settings.profiles.retain(|item| item.id != profile.id);
                        if let Some(source) = settings
                            .linked_configs
                            .iter_mut()
                            .find(|entry| profile.source.as_ref() == Some(&entry.path))
                        {
                            source.excluded.insert(profile.name.clone());
                        }
                        if this.persist(settings, cx) {
                            this.tests.retain(|_, test| {
                                if test.profile.id == profile.id {
                                    test._task.abort();
                                    false
                                } else {
                                    true
                                }
                            });
                            let mut notice = format!("已删除会话 · {}", profile.name);
                            if profile.source.is_none()
                                && let Err(error) = credentials::remove(&profile)
                            {
                                notice.push_str(&format!(" · {error:#}"));
                            }
                            this.announce(notice, cx);
                            window.close_dialog(cx);
                        }
                    });
                },
            );
            styled_dialog(dialog, 470., palette).child(
                col()
                    .child(dialog_head("删除会话", palette, false))
                    .child(div().p(px(15.)).child(format!("确定移除会话「{name}」？")))
                    .child(
                        dialog_actions(palette)
                            .child(cancel(palette, false))
                            .child(delete),
                    ),
            )
        });
    }

    pub(super) fn prompt_proxy_command(
        &mut self,
        request: ProxyCommandRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let reply = Rc::new(RefCell::new(Some(request.reply)));
        let destination = request.destination;
        let command = request.command;
        window.open_dialog(cx, move |dialog, _, cx| {
            let mono = Theme::global(cx).mono_font_family.clone();
            let cancel = reply.clone();
            let approve = reply.clone();
            styled_dialog(dialog, 620., palette)
                .on_cancel(move |_, _, _| {
                    if let Some(reply) = cancel.borrow_mut().take() {
                        let _ = reply.send(false);
                    }
                    true
                })
                .child(
                    col()
                        .child(detailed_head(
                            "确认运行 ProxyCommand",
                            "本次 SSH 连接将启动以下本地程序。仅在认可程序和参数时继续。",
                            palette,
                            false,
                        ))
                        .child(
                            col()
                                .gap(px(10.))
                                .p(px(20.))
                                .child(description_text(format!("目标：{destination}"), palette))
                                .child(
                                    div()
                                        .font_family(mono)
                                        .p(px(10.))
                                        .rounded(px(6.))
                                        .bg(palette.input)
                                        .child(command.clone()),
                                ),
                        )
                        .child(
                            dialog_actions(palette)
                                .child(
                                    button("cancel-proxy-command", "取消连接", palette)
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    primary("approve-proxy-command", "运行并连接", palette)
                                        .on_click(move |_, window, cx| {
                                            if let Some(reply) = approve.borrow_mut().take() {
                                                let _ = reply.send(true);
                                            }
                                            window.close_dialog(cx);
                                        }),
                                ),
                        ),
                )
        });
    }

    pub(super) fn prompt_trust(
        &mut self,
        request: TrustRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let TrustRequest {
            host,
            port,
            fingerprint,
            algorithm,
            known_hosts,
            previous_fingerprints,
            route,
            index,
            reply,
        } = request;
        let reply = Rc::new(RefCell::new(Some(reply)));
        let changed = !previous_fingerprints.is_empty();
        let palette = Palette::new(self.settings.light);
        let color = if changed {
            palette.danger
        } else {
            palette.warn
        };
        let confirmation = cx.new(|cx| InputState::new(window, cx).placeholder("输入主机名"));
        let refresh = refresh_on_change(&confirmation, window, cx);
        let algorithm = match algorithm.as_str() {
            "ssh-ed25519" => "ED25519（256 位）".into(),
            "ssh-rsa" => "RSA".into(),
            _ => algorithm,
        };
        let title = if changed {
            format!("{host} 的主机密钥已变化")
        } else {
            format!("确认 {host} 的主机密钥")
        };
        let address = route
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("{host}:{port}"));
        let location = crate::config::display_path(&known_hosts.to_string_lossy());
        window.open_dialog(cx, move |dialog, _, cx| {
            let _ = &refresh;
            let mono = Theme::global(cx).mono_font_family.clone();
            let text = if changed {
                "服务器可能更换了密钥，也可能存在中间人攻击。请通过可信渠道核对新指纹，确认后输入主机名更新记录。"
            } else {
                "首次连接这台主机，known_hosts 里还没有它的记录。请通过可信渠道（运维文档、云控制台）核对下面的指纹，一致后再继续。"
            };
            let make_line = |label, text: String, strong| row().items_start().gap(px(8.))
                .min_h(px(32.)).py(px(5.)).border_b_1().border_color(palette.line)
                .child(div().w(px(93.)).flex_shrink_0().text_color(palette.muted).child(label))
                .child(div().flex_1().min_w_0().font_family(mono.clone()).when(strong, |line| line.text_color(color)).child(text));
            let route_display = row().min_h(px(32.)).border_b_1().border_color(palette.line)
                .child(div().w(px(101.)).flex_shrink_0().text_color(palette.muted).child("链路"))
                .child(div().flex_1().min_w_0().text_color(palette.route).font_family(mono.clone())
                    .child(format!("本机 ─ {}", route.iter().enumerate().map(|(hop_index, address)| {
                        if hop_index == index { format!("◉ {address}") } else { format!("● {address}") }
                    }).collect::<Vec<_>>().join(" ─ "))));
            let body = col().px(px(20.)).py(px(12.))
                .child(make_line("主机", address.clone(), false))
                .child(route_display)
                .child(make_line("密钥类型", algorithm.clone(), false))
                .when(changed, |body| body.child(make_line("原 SHA256 指纹", previous_fingerprints.join("\n"), false)))
                .child(make_line("SHA256 指纹", fingerprint.clone(), true))
                .child(make_line("写入位置", location.clone(), false))
                .when(changed, |body| body.child(col().gap(px(5.)).mt(px(10.))
                    .child(description_text(format!("请输入 {host} 以确认更新"), palette))
                    .child(input(&confirmation, "输入主机名以确认", palette))));
            let decision = |value| {
                let reply = reply.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    if let Some(reply) = reply.borrow_mut().take() {
                        let _ = reply.send(value);
                    }
                    window.close_dialog(cx);
                }
            };
            let dismiss = reply.clone();
            let reject = button("cancel-trust", if changed { "断开" } else { "取消连接" }, palette)
                .on_click(decision(TrustDecision::Reject));
            let approved = !changed || confirmation.read(cx).value().trim() == host;
            let save = primary("save-host-key", if changed { "更新密钥并连接" } else { "信任并保存" }, palette)
                .disabled(!approved).on_click(decision(TrustDecision::Save));
            styled_dialog(dialog, 620., palette)
                .on_cancel(move |_, _, _| {
                    if let Some(reply) = dismiss.borrow_mut().take() { let _ = reply.send(TrustDecision::Reject); }
                    true
                })
                .child(col()
                    .child(row().items_start().gap(px(13.)).px(px(20.)).pt(px(20.))
                        .child(row().size(px(42.)).justify_center().flex_shrink_0().rounded(px(8.)).bg(color.opacity(0.16))
                            .child(icon(if changed { IconName::ShieldAlert } else { IconName::Shield }, 23., color)))
                        .child(col().gap(px(3.)).flex_1()
                            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(title.clone()))
                            .child(description_text(text, palette))))
                    .child(body)
                    .child(dialog_actions(palette).child(reject).child(div().flex_1())
                        .when(!changed, |actions| actions.child(button("trust-once", "仅本次信任", palette)
                            .on_click(decision(TrustDecision::Once))))
                        .child(save)))
        });
    }

    pub(super) fn notice_dialog(
        &mut self,
        detail: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        window.open_dialog(cx, move |dialog, _, _| {
            styled_dialog(dialog, 600., palette).child(
                col()
                    .child(dialog_head("CapTerm 活动", palette, false))
                    .child(div().p(px(15.)).child(detail.clone()))
                    .child(
                        dialog_actions(palette).child(
                            button("close-activity", "关闭", palette)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    ),
            )
        });
    }
}
