use super::*;
use iced::widget::{column, row};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Running,
    Stopped,
    Starting,
    Stopping,
    Creating,
    Updating,
    Attention,
    Cleanup,
    Unavailable,
    Blocked,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Stopped => "Stopped",
            Self::Starting => "Starting…",
            Self::Stopping => "Stopping…",
            Self::Creating => "Creating…",
            Self::Updating => "Updating…",
            Self::Attention => "Needs attention",
            Self::Cleanup => "Cleanup needed",
            Self::Unavailable => "Status unavailable",
            Self::Blocked => "Startup blocked",
        }
    }
    pub fn busy(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Stopping | Self::Creating | Self::Updating
        )
    }
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Running => Color::from_rgb8(154, 224, 183),
            Self::Starting | Self::Stopping | Self::Creating | Self::Updating => {
                Color::from_rgb8(255, 207, 112)
            }
            Self::Attention | Self::Cleanup | Self::Blocked => theme.colors.error,
            _ => theme.colors.on_surface_variant,
        }
    }
}

pub(super) fn unfinished(s: &Snapshot, vm: &Vm) -> Option<&'static str> {
    s.operations
        .iter()
        .find(|op| {
            op.vm_id == vm.id && matches!(op.phase.as_str(), "pending" | "applying" | "retrying")
        })
        .map(|_| "active")
        .or_else(|| {
            s.operations
                .iter()
                .find(|op| op.vm_id == vm.id && op.phase == "needs_review")
                .map(|_| "review")
        })
}

pub(crate) fn state(app: &App, s: &Snapshot, vm: &Vm) -> State {
    if app.snapshot_unavailable {
        return State::Unavailable;
    }
    let request = app
        .vm_request
        .as_ref()
        .filter(|(id, _)| *id == vm.id)
        .map(|(_, request)| request)
        .or_else(|| {
            s.operations
                .iter()
                .find(|op| {
                    op.vm_id == vm.id
                        && matches!(op.phase.as_str(), "pending" | "applying" | "retrying")
                })
                .map(|op| &op.request)
        });
    if s.memory_health.as_ref().is_some_and(|h| h.waiting(vm))
        && !request.is_some_and(|r| r.stops_vm())
    {
        return State::Blocked;
    }
    if let Some(request) = request {
        return match request {
            Mutation::Start { .. }
            | Mutation::State {
                desired: Lifecycle::Running,
                ..
            } => State::Starting,
            Mutation::ForceStop { .. }
            | Mutation::State {
                desired: Lifecycle::Stopped,
                ..
            } => State::Stopping,
            Mutation::Create { .. } => State::Creating,
            _ => State::Updating,
        };
    }
    if unfinished(s, vm) == Some("review")
        || vm
            .error
            .as_ref()
            .is_some_and(|e| !e.starts_with("pending:"))
    {
        return State::Attention;
    }
    match (vm.desired, vm.observed.as_str()) {
        (Lifecycle::Running, "running") => State::Running,
        (Lifecycle::Stopped, "stopped") if vm.has_compute() => State::Cleanup,
        (Lifecycle::Stopped, "stopped") => State::Stopped,
        (Lifecycle::Running, "stopped") => State::Starting,
        (Lifecycle::Stopped, "running") => State::Stopping,
        (_, "pending") => State::Creating,
        _ => State::Unavailable,
    }
}

pub(super) fn marker<'a>(state: State) -> Element<'a, Msg> {
    let hollow = matches!(state, State::Stopped | State::Unavailable);
    container(widget::space().width(10).height(10))
        .style(move |theme: &Theme| container::Style {
            background: (!hollow).then(|| state.color(theme).into()),
            border: Border {
                color: state.color(theme),
                width: 2.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        })
        .into()
}

pub(super) fn terminal(app: &App, vm: &Vm) -> Element<'static, Msg> {
    terminal_with_variant(app, vm, Variant::Filled)
}
pub(super) fn terminal_outlined(app: &App, vm: &Vm) -> Element<'static, Msg> {
    terminal_with_variant(app, vm, Variant::Outlined)
}
fn terminal_with_variant(app: &App, vm: &Vm, variant: Variant) -> Element<'static, Msg> {
    iced_m3::Button::new(
        row![
            Symbol::new(IconName::Terminal)
                .optical_size(OpticalSize::Size20)
                .icon()
                .size(20),
            typography("Open terminal", Type::LabelLarge)
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .variant(variant)
    .size(iced_m3::ButtonSize::Small)
    .on_press(Msg::Ssh(vm.id, false))
    .disabled(
        app.busy
            || app.snapshot_unavailable
            || app.vm_request.as_ref().is_some_and(|(id, _)| *id == vm.id)
            || app
                .snapshot
                .as_ref()
                .is_some_and(|s| unfinished(s, vm) == Some("active"))
            || vm.desired != Lifecycle::Running
            || vm.observed != "running"
            || vm.ssh_host_key.is_none(),
    )
    .into()
}

pub(super) fn control<'a>(app: &App, s: &Snapshot, vm: &Vm) -> Element<'a, Msg> {
    let status = state(app, s, vm);
    let blocked_review = unfinished(s, vm) == Some("review") && reviewed_stop(s, vm).is_none();
    let cleanup = vm.desired == Lifecycle::Stopped && vm.observed == "stopped" && vm.has_compute();
    let start = vm.desired == Lifecycle::Stopped && vm.observed == "stopped" && !cleanup;
    let start_block = start
        .then_some(s.memory_health.as_ref())
        .flatten()
        .filter(|h| h.blocks_start(vm));
    let label = if let Some(health) = start_block {
        health.condition.title()
    } else if status.busy() {
        status.label()
    } else if blocked_review {
        "Review the unfinished operation in this VM"
    } else if cleanup {
        "Finish stopping"
    } else if status == State::Unavailable {
        "Status unavailable"
    } else if start {
        "Start VM"
    } else {
        "Stop VM"
    };
    let content: Element<'_, Msg> = if status.busy() {
        // A stationary activity arc remains recognizable with motion disabled;
        // it is not an estimate of operation completion.
        iced_m3::circular_progress(0.72)
            .indeterminate(!app.reduced_motion)
            .size(24.0)
            .colors([Color::from_rgb8(255, 207, 112)])
            .into()
    } else {
        icon(if start {
            IconName::PlayArrow
        } else {
            IconName::Stop
        })
        .into()
    };
    let action = if start {
        Msg::Change(Mutation::Start {
            vm_id: vm.id,
            without_gpu: false,
        })
    } else if cleanup {
        Msg::Change(Mutation::State {
            vm_id: vm.id,
            desired: Lifecycle::Stopped,
        })
    } else {
        Msg::Open(Panel::Stop(vm.id))
    };
    let button = iced_m3::icon_button(content)
        .variant(Variant::Tonal)
        .on_press(action)
        .disabled(
            app.busy
                || status.busy()
                || status == State::Unavailable
                || blocked_review
                || start_block.is_some(),
        );
    iced_m3::tooltip(
        container(button).id(format!("vm-lifecycle-{}", vm.id)),
        format!("{label} · {}", vm.spec.name),
    )
    .placement(iced_m3::tooltip::Placement::Bottom)
    .into()
}

pub(super) fn reviewed_stop<'a>(s: &'a Snapshot, vm: &Vm) -> Option<&'a Operation> {
    if unfinished(s, vm) == Some("active") {
        return None;
    }
    s.operations
        .iter()
        .find(|op| op.vm_id == vm.id && op.phase == "needs_review" && op.request.stops_vm())
}
pub(super) fn can_force(app: &App, s: &Snapshot, vm: &Vm) -> bool {
    !app.busy
        && !app.snapshot_unavailable
        && app.vm_request.is_none()
        && vm.observed != "stopped"
        && vm.desired != Lifecycle::Deleted
        && (unfinished(s, vm).is_none() || reviewed_stop(s, vm).is_some())
}
pub(super) fn recovery<'a>(app: &App, s: &'a Snapshot, vm: &Vm) -> Option<Element<'a, Msg>> {
    let op = reviewed_stop(s, vm)?;
    if app.vm_request.as_ref().is_some_and(|(id, _)| *id == vm.id) {
        return None;
    }
    let stopped = vm.observed == "stopped";
    let force = matches!(op.request, Mutation::ForceStop { .. });
    let mut actions = row![
        secondary(
            if stopped {
                "Finish stopping"
            } else {
                "Keep waiting"
            },
            Msg::Change(Mutation::State {
                vm_id: vm.id,
                desired: Lifecycle::Stopped
            })
        )
        .disabled(app.busy || app.snapshot_unavailable)
    ]
    .spacing(8);
    if !stopped {
        actions = actions.push(
            danger("Force stop…", Msg::Open(Panel::ForceStop(vm.id)))
                .disabled(!can_force(app, s, vm)),
        );
    }
    Some(surface(column![
        heading(if stopped { "Shutdown cleanup hasn’t completed" } else if force { "Force stop hasn’t completed" } else { "Shutdown hasn’t completed" }),
        copy(if stopped { "The VM is off, but its compute allocations remain held until cleanup succeeds." } else { "The guest hasn’t shut down. Waiting gives it another chance to close safely. CPU, RAM and GPU allocations remain held." }),
        actions.wrap(),
    ].spacing(12), false).into())
}
