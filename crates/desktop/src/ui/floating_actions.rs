use super::*;

const HEIGHT: f32 = 56.0;
const CONTENT_GAP: f32 = 16.0;

pub(super) fn action<'a>(app: &App, s: &Snapshot) -> Option<Element<'a, Msg>> {
    let (symbol, label, message, disabled) = match app.page {
        Page::Workspaces => (
            "add",
            "New VM",
            Msg::Open(Panel::Create),
            app.busy || app.snapshot_unavailable,
        ),
        Page::Workspace(id) => {
            let vm = s.vms.iter().find(|v| v.id == id)?;
            (
                "edit",
                "Edit VM",
                Msg::Open(Panel::EditWorkspace(id)),
                !workspace::can_edit(app, s, vm),
            )
        }
        _ => return None,
    };
    Some(
        container(
            iced_m3::extended_fab(lifecycle::symbol(symbol, 24.0), label)
                .on_press(message)
                .disabled(disabled),
        )
        .id("page-fab")
        .into(),
    )
}

pub(super) fn positioned(action: Element<'_, Msg>) -> Element<'_, Msg> {
    container(action)
        .padding(iced::Padding {
            right: PAGE_EDGE_INSET,
            bottom: PAGE_EDGE_INSET,
            ..Default::default()
        })
        .align_right(Length::Fill)
        .align_bottom(Length::Fill)
        .into()
}

/// The scroller already ends at the page's outer padding. Reserve the remainder
/// so its final content can move above the button with a full 16px gap.
pub(super) fn clearance(outer_padding: f32) -> f32 {
    (PAGE_EDGE_INSET + HEIGHT + CONTENT_GAP - outer_padding).max(16.0)
}
