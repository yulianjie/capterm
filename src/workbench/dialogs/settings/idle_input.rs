use super::*;

pub(super) struct IdleInput {
    state: Entity<InputState>,
    error: Rc<Cell<Option<&'static str>>>,
    _subscription: Subscription,
}

impl IdleInput {
    pub(super) fn new(settings: &Settings, view: &View, window: &mut Window, cx: &mut App) -> Self {
        let mut committed = settings.idle_threshold.max(1);
        let state = cx.new(|cx| InputState::new(window, cx).default_value(committed.to_string()));
        let error = Rc::new(Cell::new(None));
        let feedback = error.clone();
        let view = view.clone();
        let subscription =
            window.subscribe(&state, cx, move |state, event, window, cx| match event {
                InputEvent::Change => {
                    if feedback.take().is_some() {
                        window.refresh();
                    }
                }
                InputEvent::Blur | InputEvent::PressEnter { .. } => {
                    let value = state.read(cx).value();
                    let Some(seconds) = value.trim().parse::<u64>().ok().filter(|&value| value > 0)
                    else {
                        feedback.set(Some("请输入正整数。"));
                        window.refresh();
                        return;
                    };
                    if seconds == committed {
                        return;
                    }
                    let saved = view.update(cx, |this, cx| {
                        let mut settings = this.settings.clone();
                        settings.idle_threshold = seconds;
                        let saved = this.persist(settings, cx);
                        if saved {
                            this.apply_terminal_preferences(cx);
                        }
                        saved
                    });
                    if let Ok(saved) = saved {
                        feedback.set((!saved).then_some("无法保存设置。"));
                        if saved {
                            committed = seconds;
                        }
                        window.refresh();
                    }
                }
                InputEvent::Focus => {}
            });
        Self {
            state,
            error,
            _subscription: subscription,
        }
    }

    pub(super) fn render(&self, palette: Palette) -> Div {
        let error = self.error.get();
        col()
            .w(px(120.))
            .flex_shrink_1()
            .gap(px(3.))
            .child(
                input(&self.state, "空闲间隔线秒数", palette)
                    .suffix(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .text_color(palette.dim)
                            .child("秒"),
                    )
                    .when(error.is_some(), |field| field.border_color(palette.danger)),
            )
            .when_some(error, |field, error| {
                field.child(
                    div()
                        .text_size(px(11.))
                        .text_color(palette.danger)
                        .child(error),
                )
            })
    }
}
