use super::{Palette, Settings, View, button, col, row, setting_line, settings_section};
use crate::shortcuts::{ShortcutAction, ShortcutBindings};
use crate::workbench::Workbench;
use gpui_kit::component::Disableable;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Div, FocusHandle, Subscription, Window, div, px};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(super) struct ShortcutRecorder {
    focus: FocusHandle,
    action: Cell<Option<ShortcutAction>>,
    error: RefCell<Option<(Option<ShortcutAction>, String)>>,
}

impl ShortcutRecorder {
    pub(super) fn new(cx: &mut Context<Workbench>) -> (Rc<Self>, Subscription) {
        let recorder = Rc::new(Self {
            focus: cx.focus_handle(),
            action: Cell::new(None),
            error: RefCell::new(None),
        });
        let capture = Rc::downgrade(&recorder);
        let view = cx.weak_entity();
        let subscription = cx.intercept_keystrokes(move |event, window, cx| {
            let Some(recorder) = capture.upgrade() else {
                return;
            };
            let Some(action) = recorder
                .action
                .get()
                .filter(|_| recorder.focus.is_focused(window))
            else {
                return;
            };
            let key = &event.keystroke;
            if key.is_modifier_key() {
                return;
            }
            if key.key == "escape" {
                recorder.cancel();
            } else if recorder.update(&view, Some(action), cx, |bindings| {
                bindings.set(action, key)
            }) {
                recorder.action.set(None);
            }
            window.prevent_default();
            cx.stop_propagation();
            window.refresh();
        });
        (recorder, subscription)
    }

    pub(super) fn cancel(&self) {
        self.action.set(None);
        self.error.borrow_mut().take();
    }

    fn update(
        &self,
        view: &View,
        action: Option<ShortcutAction>,
        cx: &mut App,
        update: impl FnOnce(&mut ShortcutBindings) -> Result<(), String>,
    ) -> bool {
        let result = view
            .update(cx, |this, cx| {
                let mut settings = this.settings.clone();
                update(&mut settings.shortcuts)?;
                if this.persist(settings, cx) {
                    Ok(())
                } else {
                    Err(this.notice.clone())
                }
            })
            .unwrap_or_else(|error| Err(error.to_string()));
        let saved = result.is_ok();
        self.error
            .replace(result.err().map(|error| (action, error)));
        saved
    }

    fn error(&self, action: Option<ShortcutAction>, palette: Palette) -> Option<Div> {
        let error = self.error.borrow();
        let (_, message) = error.as_ref().filter(|(owner, _)| *owner == action)?;
        Some(
            div()
                .mb(px(6.))
                .text_size(px(11.5))
                .text_color(palette.warn)
                .child(message.clone()),
        )
    }

    pub(super) fn render(
        self: &Rc<Self>,
        settings: &Settings,
        view: &View,
        palette: Palette,
        window: &Window,
    ) -> Div {
        if self.action.get().is_some() && !self.focus.is_focused(window) {
            self.cancel();
        }
        let bindings = &settings.shortcuts;
        let reset_all = {
            let recorder = self.clone();
            let view = view.clone();
            button("shortcuts-reset-all", "恢复全部默认", palette)
                .disabled(!bindings.has_customizations())
                .on_click(move |_, window, cx| {
                    recorder.cancel();
                    recorder.update(&view, None, cx, |bindings| {
                        *bindings = ShortcutBindings::default();
                        Ok(())
                    });
                    window.refresh();
                })
        };
        let (intro, clipboard) = if cfg!(target_os = "macos") {
            (
                "点击组合键后按下新组合键。工作台使用 Command，切换标签也可用 Ctrl+Tab。",
                "⌘C / ⌘V",
            )
        } else {
            (
                "点击组合键后按下新组合键。工作台主要使用 Ctrl+Shift，保留终端程序的 Ctrl 组合键。",
                "Ctrl+Shift+C / Ctrl+Shift+V",
            )
        };
        settings_section("快捷键", intro, palette).child(
            col()
                .id("shortcut-recorder")
                .track_focus(&self.focus)
                .child(row().justify_end().pb(px(8.)).child(reset_all))
                .children(self.error(None, palette))
                .children(ShortcutAction::ALL.map(|action| {
                    let index = action as usize;
                    let recording = self.action.get() == Some(action);
                    let label = if recording {
                        "按下新组合键…".to_owned()
                    } else {
                        bindings.label(action)
                    };
                    let capture = self.clone();
                    let edit = button(("shortcut-edit", index), label, palette)
                        .h(px(28.))
                        .accessibility_label(format!("修改{}", action.label()))
                        .when(recording, |edit| edit.border_color(palette.accent))
                        .on_click(move |_, window, cx| {
                            capture.cancel();
                            capture.action.set(Some(action));
                            capture.focus.focus(window, cx);
                            window.refresh();
                        });
                    let reset = {
                        let recorder = self.clone();
                        let view = view.clone();
                        button(("shortcut-reset", index), "恢复", palette)
                            .h(px(28.))
                            .accessibility_label(format!("恢复{}的默认快捷键", action.label()))
                            .on_click(move |_, window, cx| {
                                recorder.cancel();
                                recorder.update(&view, Some(action), cx, |bindings| {
                                    bindings.reset(action)
                                });
                                window.refresh();
                            })
                    };
                    col()
                        .child(
                            setting_line(
                                action.label(),
                                if recording {
                                    "按 Esc 取消"
                                } else {
                                    action.detail()
                                },
                                row()
                                    .justify_end()
                                    .gap(px(5.))
                                    .flex_wrap()
                                    .child(edit)
                                    .when(bindings.customized(action), |line| line.child(reset)),
                                palette,
                            )
                            .py(px(8.)),
                        )
                        .children(self.error(Some(action), palette))
                }))
                .child(setting_line(
                    "终端复制 / 粘贴",
                    "固定操作",
                    clipboard,
                    palette,
                ))
                .child(setting_line("终端重连", "SSH 已断开时", "Enter", palette))
                .child(setting_line(
                    "浏览命令历史",
                    "命令输入栏内",
                    "↑ / ↓",
                    palette,
                ))
                .child(setting_line("关闭对话框", "固定操作", "Esc", palette)),
        )
    }
}
