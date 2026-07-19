use crate::prelude::*;

#[derive(IntoElement)]
pub struct ListSeparator;

impl RenderOnce for ListSeparator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .h_px()
            .w_full()
            .my(DynamicSpacing::Base06.rems(cx))
            .bg(window.theme(cx).colors().border_variant)
    }
}
