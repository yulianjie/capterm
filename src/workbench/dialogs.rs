use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::{anyhow, ensure};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::{InputContentType, InputState, Textarea, TextareaState};
use gpui_kit::component::{Disableable, Theme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::Workbench;
use super::ui::{Palette, badge, button, col, input, pane_head, primary, row, target, tool};
use crate::config::{self, ConnectionProfile, Hop};
use crate::ssh::{RemoteEntry, TrustRequest};

#[derive(Default)]
pub(super) struct FormStatus {
    busy: Cell<bool>,
    error: RefCell<Option<String>>,
}

impl FormStatus {
    fn start(&self, window: &mut Window) {
        self.busy.set(true);
        self.error.borrow_mut().take();
        window.refresh();
    }

    pub(super) fn fail(&self, error: impl Into<String>, window: &mut Window) {
        self.busy.set(false);
        self.error.replace(Some(error.into()));
        window.refresh();
    }

    fn error(&self, palette: Palette) -> Option<Div> {
        self.error.borrow().as_ref().map(|error| {
            div()
                .text_size(px(12.))
                .text_color(palette.warn)
                .child(error.clone())
        })
    }
}

fn styled_dialog(dialog: Dialog, width: f32, palette: Palette) -> Dialog {
    dialog
        .w(px(width))
        .centered()
        .p_0()
        .gap_0()
        .bg(palette.panel)
        .border_color(palette.line)
        .rounded(px(11.))
        .close_button(false)
        .overlay_closable(false)
        .entrance(DialogEntrance::None)
        .text_color(palette.ink)
        .text_size(px(13.))
}

fn dialog_head(title: impl Into<SharedString>, palette: Palette, busy: bool) -> Div {
    pane_head(palette)
        .child(div().truncate().child(title.into()))
        .child(
            tool("dialog-x", "×", "Close", palette)
                .disabled(busy)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
}

fn dialog_actions(palette: Palette) -> Div {
    row()
        .justify_end()
        .gap(px(8.))
        .px(px(15.))
        .py(px(11.))
        .border_t_1()
        .border_color(palette.line)
}

fn cancel(palette: Palette, busy: bool) -> gpui_kit::component::button::Button {
    button("dialog-cancel", "Cancel", palette)
        .disabled(busy)
        .on_click(|_, window, cx| window.close_dialog(cx))
}

fn field(label: &'static str, control: impl IntoElement, palette: Palette) -> Div {
    col()
        .gap(px(4.))
        .child(div().text_color(palette.muted).child(label))
        .child(control)
}

const PROFILE_FIELDS: [(&str, &str); 6] = [
    ("Display name", "Session name"),
    ("Host or SSH config alias", "host.example.com"),
    ("User", "SSH username"),
    ("Port", "22"),
    ("Jump host", "user@jump.example.com"),
    ("Identity file", "~/.ssh/id_ed25519"),
];

impl Workbench {
    pub(super) fn profile_dialog(
        &mut self,
        profile: Option<ConnectionProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let editing = profile.is_some();
        let defaults = profile.as_ref().map_or_else(
            || {
                [
                    "".into(),
                    "".into(),
                    "".into(),
                    "22".into(),
                    "".into(),
                    "".into(),
                ]
            },
            |profile| {
                [
                    profile.name.clone(),
                    profile.host.clone(),
                    profile.user.clone(),
                    profile.port.to_string(),
                    profile
                        .jumps
                        .iter()
                        .map(format_hop)
                        .collect::<Vec<_>>()
                        .join(", "),
                    profile.identity_files.join("; "),
                ]
            },
        );
        let states = PROFILE_FIELDS
            .into_iter()
            .zip(defaults)
            .map(|((_, placeholder), default)| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(default)
                })
            })
            .collect::<Vec<_>>();
        let status = Rc::new(FormStatus::default());
        let view = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let body = col()
                .gap(px(12.))
                .p(px(15.))
                .children(
                    PROFILE_FIELDS
                        .into_iter()
                        .zip(&states)
                        .map(|((label, _), state)| {
                            field(label, input(state, label, palette), palette)
                        }),
                )
                .children(status.error(palette));
            let view = view.clone();
            let profile = profile.clone();
            let states = states.clone();
            let status = status.clone();
            let save =
                primary("save-profile", "Save session", palette).on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        match this.collect_profile(profile.as_ref(), &states, cx) {
                            Ok(profile) => {
                                let mut settings = this.settings.clone();
                                let name = profile.name.clone();
                                if let Some(existing) = settings
                                    .profiles
                                    .iter_mut()
                                    .find(|item| item.id == profile.id)
                                {
                                    *existing = profile;
                                } else {
                                    settings.profiles.push(profile);
                                }
                                if this.persist(settings, cx) {
                                    this.announce(format!("Saved session · {name}"), cx);
                                    window.close_dialog(cx);
                                } else {
                                    status.fail(this.notice.clone(), window);
                                }
                            }
                            Err(error) => status.fail(error.to_string(), window),
                        }
                    });
                });
            styled_dialog(dialog, 470., palette).child(
                col()
                    .child(dialog_head(
                        if editing {
                            "Edit SSH session"
                        } else {
                            "New SSH session"
                        },
                        palette,
                        false,
                    ))
                    .child(body)
                    .child(
                        dialog_actions(palette)
                            .child(cancel(palette, false))
                            .child(save),
                    ),
            )
        });
    }

    fn collect_profile(
        &self,
        previous: Option<&ConnectionProfile>,
        fields: &[Entity<InputState>],
        cx: &App,
    ) -> anyhow::Result<ConnectionProfile> {
        let values = fields
            .iter()
            .map(|field| field.read(cx).value().trim().to_owned())
            .collect::<Vec<_>>();
        let [name, host, user, port, jumps, keys] = values.as_slice() else {
            unreachable!()
        };
        ensure!(
            !host.is_empty(),
            "Enter a hostname or an imported SSH alias."
        );
        let (resolved_host, host_user, host_port) = config::parse_hop(host)?;
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| anyhow!("Port must be between 1 and 65535."))?;
        ensure!(
            !user.contains(['/', '@', ':']) && !user.contains(char::is_whitespace),
            "Invalid SSH username."
        );
        let local_user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let alias = previous
            .is_none()
            .then(|| {
                self.settings.profiles.iter().find(|profile| {
                    profile.source.is_some() && profile.name.eq_ignore_ascii_case(resolved_host)
                })
            })
            .flatten();
        let user = if user.is_empty() {
            host_user
                .or_else(|| alias.map(|alias| alias.user.as_str()))
                .unwrap_or(&local_user)
        } else {
            user
        };
        ensure!(!user.is_empty(), "Enter an SSH username.");
        let host = alias.map_or(resolved_host, |alias| &alias.host).to_owned();
        let port = host_port
            .or_else(|| alias.filter(|_| port == 22).map(|alias| alias.port))
            .unwrap_or(port);
        let identity_files = if keys.is_empty() {
            alias.map_or_else(Vec::new, |alias| alias.identity_files.clone())
        } else {
            keys.split(';')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let jumps = if jumps.is_empty() {
            alias.map_or_else(Vec::new, |alias| alias.jumps.clone())
        } else {
            jumps
                .split(',')
                .map(|jump| {
                    let (host, user, port) = config::parse_hop(jump)?;
                    let alias = self.settings.profiles.iter().find(|profile| {
                        profile.source.is_some() && profile.name.eq_ignore_ascii_case(host)
                    });
                    let host = alias.map_or(host, |alias| &alias.host);
                    let user = user
                        .or_else(|| alias.map(|alias| alias.user.as_str()))
                        .unwrap_or(&local_user);
                    let port = port.or_else(|| alias.map(|alias| alias.port)).unwrap_or(22);
                    let identity_files = previous
                        .and_then(|previous| {
                            previous.jumps.iter().find(|hop| {
                                hop.host == host && hop.user == user && hop.port == port
                            })
                        })
                        .map(|hop| hop.identity_files.clone())
                        .or_else(|| alias.map(|alias| alias.identity_files.clone()))
                        .unwrap_or_default();
                    Ok(Hop {
                        host: host.to_owned(),
                        user: user.to_owned(),
                        port,
                        identity_files,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        };
        ensure!(jumps.len() <= 8, "Use at most eight jump hosts.");
        Ok(ConnectionProfile {
            id: previous.map_or_else(
                || uuid::Uuid::new_v4().to_string(),
                |profile| profile.id.clone(),
            ),
            name: if name.is_empty() {
                resolved_host.to_owned()
            } else {
                name.clone()
            },
            host,
            user: user.to_owned(),
            port,
            identity_files,
            jumps,
            source: previous.and_then(|profile| profile.source.clone()),
        })
    }

    pub(super) fn import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let palette = Palette::new(self.settings.light);
        let path = cx
            .new(|cx| InputState::new(window, cx).default_value(self.settings.config_path.clone()));
        let view = cx.weak_entity();
        let status = Rc::new(FormStatus::default());
        window.open_dialog(cx, move |dialog, _, _| {
            let busy = status.busy.get();
            let body = col()
                .gap(px(12.))
                .p(px(15.))
                .child(field(
                    "OpenSSH config path",
                    input(&path, "OpenSSH config path", palette).disabled(busy),
                    palette,
                ))
                .child(
                    div()
                        .text_color(palette.dim)
                        .child("Named hosts and their jump routes will appear in Saved sessions."),
                )
                .children(status.error(palette));
            let on_cancel_status = status.clone();
            let on_import_status = status.clone();
            let view = view.clone();
            let path = path.clone();
            let import = primary(
                "import-sessions",
                if busy {
                    "Importing…"
                } else {
                    "Import sessions"
                },
                palette,
            )
            .disabled(busy)
            .on_click(move |_, window, cx| {
                let entered = path.read(cx).value().trim().to_owned();
                if entered.is_empty() {
                    on_import_status.fail("Enter the path to an OpenSSH config file.", window);
                    return;
                }
                let status = on_import_status.clone();
                status.start(window);
                let _ = view.update(cx, |this, cx| {
                    this.import_profiles(entered, status, window, cx)
                });
            });
            styled_dialog(dialog, 470., palette)
                .on_cancel(move |_, _, _| !on_cancel_status.busy.get())
                .child(
                    col()
                        .child(dialog_head("Import SSH config", palette, busy))
                        .child(body)
                        .child(
                            dialog_actions(palette)
                                .child(cancel(palette, busy))
                                .child(import),
                        ),
                )
        });
    }

    fn import_profiles(
        &mut self,
        path: String,
        status: Rc<FormStatus>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let import_path = path.clone();
        let operation = cx
            .background_executor()
            .spawn(async move { config::import(&import_path) });
        cx.spawn_in(window, async move |view, cx| {
            let result = operation.await;
            let _ = view.update_in(cx, |this, window, cx| match result {
                Ok(result) => {
                    let count = result.profiles.len();
                    if count == 0 {
                        let detail = result.warnings.join(" · ");
                        status.fail(format!("No supported named hosts found. {detail}"), window);
                        return;
                    }
                    let mut settings = this.settings.clone();
                    settings.config_path = path;
                    for profile in result.profiles {
                        if let Some(existing) = settings
                            .profiles
                            .iter_mut()
                            .find(|saved| saved.id == profile.id)
                        {
                            *existing = profile;
                        } else {
                            settings.profiles.push(profile);
                        }
                    }
                    if this.persist(settings, cx) {
                        let mut notice = format!("Imported {count} session(s)");
                        if !result.warnings.is_empty() {
                            notice.push_str(&format!(" · {}", result.warnings.join(" · ")));
                        }
                        this.announce(notice, cx);
                        window.close_dialog(cx);
                    } else {
                        status.fail(this.notice.clone(), window);
                    }
                }
                Err(error) => status.fail(format!("Import failed · {error:#}"), window),
            });
        })
        .detach();
    }

    pub(super) fn password_dialog(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.credential_dialog(profile, false, window, cx);
    }

    pub(super) fn test_with_password(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.credential_dialog(profile, true, window, cx);
    }

    fn credential_dialog(
        &mut self,
        profile: ConnectionProfile,
        test: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("SSH password")
        });
        let view = cx.weak_entity();
        let status = Rc::new(FormStatus::default());
        window.open_dialog(cx, move |dialog, _, _| {
            let body = col()
                .gap(px(12.))
                .p(px(15.))
                .child(div().text_color(palette.muted).child(format!(
                    "{} {}",
                    if test {
                        "Test connection to"
                    } else {
                        "Connect to"
                    },
                    target(&profile)
                )))
                .child(field(
                    "Password",
                    input(&password, "SSH password", palette)
                        .content_type(InputContentType::Password),
                    palette,
                ))
                .children(status.error(palette));
            let view = view.clone();
            let password = password.clone();
            let profile = profile.clone();
            let status = status.clone();
            let connect = primary(
                "connect-password",
                if test { "Test connection" } else { "Connect" },
                palette,
            )
            .on_click(move |_, window, cx| {
                let credential = password.read(cx).value().to_string();
                if credential.is_empty() {
                    status.fail("Enter the SSH password.", window);
                    return;
                }
                password.update(cx, |password, cx| password.clean(window, cx));
                window.close_dialog(cx);
                let _ = view.update(cx, |this, cx| {
                    if test {
                        this.test(profile.clone(), Some(credential), cx);
                    } else {
                        this.connect(profile.clone(), Some(credential), window, cx);
                    }
                });
            });
            styled_dialog(dialog, 470., palette).child(
                col()
                    .child(dialog_head("SSH password", palette, false))
                    .child(body)
                    .child(
                        dialog_actions(palette)
                            .child(cancel(palette, false))
                            .child(connect),
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
            let delete = primary("delete-saved-profile", "Delete session", palette).on_click(
                move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        let mut settings = this.settings.clone();
                        settings.profiles.retain(|item| item.id != profile.id);
                        if this.persist(settings, cx) {
                            this.tests.retain(|_, (id, task)| {
                                if *id == profile.id {
                                    task.abort();
                                    false
                                } else {
                                    true
                                }
                            });
                            this.announce(format!("Deleted saved session · {}", profile.name), cx);
                            window.close_dialog(cx);
                        }
                    });
                },
            );
            styled_dialog(dialog, 470., palette).child(
                col()
                    .child(dialog_head("Delete saved session", palette, false))
                    .child(
                        div()
                            .p(px(15.))
                            .child(format!("Remove {name} from Saved sessions?")),
                    )
                    .child(
                        dialog_actions(palette)
                            .child(cancel(palette, false))
                            .child(delete),
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
        let text = format!(
            "{}:{}\n\n{}\n\nVerify this fingerprint with the server administrator before trusting the host.",
            request.host, request.port, request.fingerprint
        );
        let result = window.prompt(
            PromptLevel::Warning,
            "Verify SSH host key",
            Some(&text),
            &["Cancel", "Trust host"],
            cx,
        );
        cx.spawn(async move |_, _| {
            let trusted = matches!(result.await, Ok(1));
            let _ = request.reply.send(trusted);
        })
        .detach();
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
                    .child(dialog_head("CapTerm activity", palette, false))
                    .child(div().p(px(15.)).child(detail.clone()))
                    .child(
                        dialog_actions(palette).child(
                            button("close-activity", "Close", palette)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                    ),
            )
        });
    }

    pub(super) fn editor_dialog(
        &mut self,
        id: u64,
        entry: RemoteEntry,
        original: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = Palette::new(self.settings.light);
        let mono = Theme::global(cx).mono_font_family.clone();
        let text = cx.new(|cx| TextareaState::new(window, cx).default_value(original.clone()));
        let status = Rc::new(FormStatus::default());
        let view = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let busy = status.busy.get();
            let heading = pane_head(palette)
                .child(
                    row()
                        .gap(px(8.))
                        .child(div().truncate().child(entry.name.clone()))
                        .child(badge("UTF-8", palette, mono.clone())),
                )
                .child(
                    tool("editor-x", "×", "Close", palette)
                        .disabled(busy)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                );
            let body = col()
                .gap(px(12.))
                .p(px(15.))
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_color(palette.dim)
                        .truncate()
                        .child(entry.path.clone()),
                )
                .child(
                    Textarea::new(&text)
                        .aria_label("Remote text file")
                        .w_full()
                        .h(px(250.))
                        .disabled(busy)
                        .font_family(mono.clone())
                        .text_size(px(13.))
                        .line_height(px(21.))
                        .bg(palette.base)
                        .border_color(palette.line)
                        .rounded(px(6.)),
                )
                .children(status.error(palette));
            let on_cancel_status = status.clone();
            let save_status = status.clone();
            let download_view = view.clone();
            let download_entry = entry.clone();
            let save_view = view.clone();
            let save_path = entry.path.clone();
            let original = original.clone();
            let text = text.clone();
            let save = primary(
                "save-remote-text",
                if busy { "Saving…" } else { "Save to host" },
                palette,
            )
            .disabled(busy)
            .on_click(move |_, window, cx| {
                let edited = text.read(cx).value().to_string();
                if edited == original {
                    window.close_dialog(cx);
                    return;
                }
                save_status.start(window);
                let _ = save_view.update(cx, |this, cx| {
                    this.save_entry(
                        id,
                        save_path.clone(),
                        (original.clone(), edited),
                        save_status.clone(),
                        window,
                        cx,
                    );
                });
            });
            styled_dialog(dialog, 760., palette)
                .on_cancel(move |_, _, _| !on_cancel_status.busy.get())
                .child(
                    col().child(heading).child(body).child(
                        dialog_actions(palette)
                            .child(
                                button("download-editor-file", "Download", palette)
                                    .disabled(busy)
                                    .on_click(move |_, window, cx| {
                                        let _ = download_view.update(cx, |this, cx| {
                                            this.pick_download(
                                                id,
                                                download_entry.clone(),
                                                window,
                                                cx,
                                            )
                                        });
                                    }),
                            )
                            .child(cancel(palette, busy))
                            .child(save),
                    ),
                )
        });
    }
}

fn format_hop(hop: &Hop) -> String {
    let host = if hop.host.contains(':') {
        format!("[{}]", hop.host)
    } else {
        hop.host.clone()
    };
    format!("{}@{}:{}", hop.user, host, hop.port)
}
