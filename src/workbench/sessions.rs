use std::fs::File;
use std::io::ErrorKind;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{AuthMethod, ConnectionProfile, LocalShell, Session, Workbench, config};
use gpui_kit::{Context, Window};

#[derive(Default, PartialEq, Eq, Serialize, Deserialize)]
struct SessionSnapshot {
    tabs: Vec<SavedTab>,
    active: Option<usize>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SavedTab {
    Local {
        shell: LocalShell,
    },
    Ssh {
        profile_id: String,
        password_override: bool,
    },
    QuickSsh {
        profile: Box<ConnectionProfile>,
    },
}

impl SessionSnapshot {
    fn load() -> Result<Self> {
        let path = config::session_state_path()?;
        match File::open(&path) {
            Ok(file) => {
                serde_json::from_reader(file).with_context(|| format!("读取 {}", path.display()))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("打开 {}", path.display())),
        }
    }

    fn capture(view: &Workbench) -> Self {
        Self {
            tabs: view
                .tabs
                .iter()
                .map(|tab| match &tab.session {
                    Session::Local(shell) => SavedTab::Local { shell: *shell },
                    Session::Ssh(profile) if profile.id.starts_with("quick:") => {
                        SavedTab::QuickSsh {
                            profile: Box::new(profile.clone()),
                        }
                    }
                    Session::Ssh(profile) => SavedTab::Ssh {
                        profile_id: profile.id.clone(),
                        password_override: tab.ssh_password_auth
                            && profile.auth != AuthMethod::Password,
                    },
                })
                .collect(),
            active: view.tabs.iter().position(|tab| Some(tab.id) == view.active),
        }
    }
}

impl Workbench {
    pub(super) fn restore_on_open(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.load_error.is_some() {
            return;
        }
        let snapshot = match SessionSnapshot::load() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.announce(format!("无法读取上次的会话：{error:#}"), cx);
                return;
            }
        };
        let view = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = view.update(cx, |view, cx| {
                view.restore_tabs(&snapshot, window, cx);
                // The entity is released on window close and during GPUI's app shutdown.
                view._subscriptions.push(cx.on_release(move |view, _| {
                    let current = SessionSnapshot::capture(view);
                    if current != snapshot
                        && let Err(error) = config::save_json("sessions.json", &current)
                    {
                        eprintln!("无法保存打开的会话：{error:#}");
                    }
                }));
            });
        });
    }

    fn restore_tabs(
        &mut self,
        snapshot: &SessionSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut active = None;
        for (index, entry) in snapshot.tabs.iter().enumerate() {
            let id = match entry {
                SavedTab::Local { shell } if cfg!(windows) => {
                    self.start_local_tab(*shell, window, cx)
                }
                SavedTab::Local { .. } => continue,
                SavedTab::Ssh {
                    profile_id,
                    password_override,
                } => {
                    let Some(profile) = self
                        .settings
                        .profiles
                        .iter()
                        .find(|profile| &profile.id == profile_id)
                        .cloned()
                    else {
                        continue;
                    };
                    self.restore_ssh_tab(profile, *password_override, window, cx)
                }
                SavedTab::QuickSsh { profile } => {
                    self.restore_ssh_tab((**profile).clone(), false, window, cx)
                }
            };
            if Some(index) == snapshot.active {
                active = Some(id);
            }
        }
        if let Some(id) = active.or_else(|| self.tabs.first().map(|tab| tab.id)) {
            self.select_tab(id, window, cx);
        }
    }

    fn restore_ssh_tab(
        &mut self,
        profile: ConnectionProfile,
        password_override: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        let requires_password = password_override || profile.auth == AuthMethod::Password;
        let result = if requires_password {
            match self.stored_ssh_password(&profile) {
                Ok(Some(password)) => {
                    self.start_ssh_tab(profile.clone(), Some(password), window, cx)
                }
                Ok(None) => Err(anyhow::anyhow!("重新连接时请输入 SSH 密码。")),
                Err(error) => Err(error),
            }
        } else {
            self.start_ssh_tab(profile.clone(), None, window, cx)
        };
        match result {
            Ok(id) => id,
            Err(error) => {
                let id = self.add_tab(Session::Ssh(profile), requires_password, window, cx);
                if let Some(tab) = self.tab(id) {
                    tab.terminal.update(cx, |terminal, cx| {
                        terminal.system_message(format!("会话标签已恢复 · {error:#}"), cx);
                    });
                }
                id
            }
        }
    }
}
