use homecloud_core::{
    Request, Response, VirtualizationGuestIdentity, VirtualizationGuestKind,
    VirtualizationInventory, VirtualizationOperation, VirtualizationOperationPhase,
    VirtualizationPowerAction,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Page {
    #[default]
    Overview,
    Guests,
    Storage,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GuestIdentity {
    pub node: String,
    pub vmid: u32,
}

#[derive(Debug, Clone)]
pub(super) enum Event {
    Navigate(Page),
    Inspect(GuestIdentity),
    ReviewPower(VirtualizationGuestIdentity, VirtualizationPowerAction),
    Confirm(Uuid),
    Cancel(Uuid),
    CheckRequest(Uuid),
    RetryRequest(Uuid),
    ReviewAcknowledgement(Uuid),
    Operations {
        session: Uuid,
        generation: u64,
        revision: u64,
        response: Result<Vec<VirtualizationOperation>, String>,
    },
    Submitted(Uuid, Uuid, Result<VirtualizationOperation, String>),
    Rejected(Uuid, Uuid, String),
    Acknowledged(Uuid, Uuid, Result<VirtualizationOperation, String>),
}

impl Event {
    pub fn submitted(session: Uuid, key: Uuid, response: anyhow::Result<Response>) -> Self {
        match response {
            Ok(Response::VirtualizationOperation(operation)) => {
                Self::Submitted(session, key, Ok(operation))
            }
            Ok(Response::Error { message }) => Self::Rejected(session, key, message),
            Ok(_) => Self::Submitted(
                session,
                key,
                Err("Unexpected power submission reply".into()),
            ),
            Err(error) => Self::Submitted(session, key, Err(error.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PowerRequest {
    pub key: Uuid,
    pub guest: VirtualizationGuestIdentity,
    pub action: VirtualizationPowerAction,
}

#[derive(Debug, Clone)]
pub(super) struct PendingPower {
    pub request: PowerRequest,
    pub in_flight: bool,
    pub error: Option<String>,
    pub checked_missing: bool,
}

#[derive(Debug, Clone)]
pub(super) enum Confirmation {
    Power(PowerRequest),
    Retry { token: Uuid, request: PowerRequest },
    Acknowledge { token: Uuid, operation_id: Uuid },
}

impl Confirmation {
    pub fn token(&self) -> Uuid {
        match self {
            Self::Power(request) => request.key,
            Self::Retry { token, .. } | Self::Acknowledge { token, .. } => *token,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Effect {
    None,
    Submit(PowerRequest),
    RefreshOperations,
    Acknowledge(Uuid),
}

pub(super) struct Cockpit {
    pub page: Page,
    pub inventory: Option<VirtualizationInventory>,
    pub selected_guest: Option<GuestIdentity>,
    pub received_at: Option<u64>,
    pub error: Option<String>,
    pub session: Uuid,
    pub operations: Vec<VirtualizationOperation>,
    pub operations_received_at: Option<u64>,
    pub operations_error: Option<String>,
    pub operations_loading: bool,
    pub inventory_loading: bool,
    pub confirmation: Option<Confirmation>,
    pub pending: Option<PendingPower>,
    pub acknowledging: Option<Uuid>,
    pub power_error: Option<String>,
    operations_generation: u64,
    operations_revision: u64,
}

impl Default for Cockpit {
    fn default() -> Self {
        Self {
            page: Page::default(),
            inventory: None,
            selected_guest: None,
            received_at: None,
            error: None,
            session: Uuid::new_v4(),
            operations: Vec::new(),
            operations_received_at: None,
            operations_error: None,
            operations_loading: false,
            inventory_loading: false,
            confirmation: None,
            pending: None,
            acknowledging: None,
            power_error: None,
            operations_generation: 0,
            operations_revision: 0,
        }
    }
}

impl Cockpit {
    pub fn update(&mut self, event: Event) -> Effect {
        let now = crate::unix_now();
        match event {
            Event::Navigate(page) => {
                self.page = page;
                self.selected_guest = None;
                self.confirmation = None;
            }
            Event::Inspect(guest) => {
                self.page = Page::Guests;
                self.selected_guest = Some(guest);
                self.confirmation = None;
            }
            Event::ReviewPower(guest, action) => {
                if let Some(reason) = self.power_block_reason(&guest, action, now, false) {
                    self.power_error = Some(reason.into());
                } else {
                    self.power_error = None;
                    self.confirmation = Some(Confirmation::Power(PowerRequest {
                        key: Uuid::new_v4(),
                        guest,
                        action,
                    }));
                }
            }
            Event::Cancel(token) => {
                if self
                    .confirmation
                    .as_ref()
                    .is_some_and(|review| review.token() == token)
                {
                    self.confirmation = None;
                }
            }
            Event::Confirm(token) => {
                let Some(review) = self
                    .confirmation
                    .clone()
                    .filter(|review| review.token() == token)
                else {
                    return Effect::None;
                };
                match review {
                    Confirmation::Power(request) | Confirmation::Retry { request, .. } => {
                        let recovering = self
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.request == request);
                        if let Some(reason) =
                            self.power_block_reason(&request.guest, request.action, now, recovering)
                        {
                            self.power_error = Some(reason.into());
                            return Effect::None;
                        }
                        self.pending = Some(PendingPower {
                            request: request.clone(),
                            in_flight: true,
                            error: None,
                            checked_missing: false,
                        });
                        self.confirmation = None;
                        self.power_error = None;
                        self.operations_revision = self.operations_revision.saturating_add(1);
                        return Effect::Submit(request);
                    }
                    Confirmation::Acknowledge { operation_id, .. } => {
                        if !self.can_acknowledge(operation_id, now) {
                            self.power_error = Some(
                                "Refresh operations before releasing an Unknown safety hold."
                                    .into(),
                            );
                            return Effect::None;
                        }
                        self.confirmation = None;
                        self.acknowledging = Some(operation_id);
                        self.power_error = None;
                        self.operations_revision = self.operations_revision.saturating_add(1);
                        return Effect::Acknowledge(operation_id);
                    }
                }
            }
            Event::CheckRequest(key) => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.request.key == key && !pending.in_flight)
                    && !self.operations_loading
                {
                    return Effect::RefreshOperations;
                }
            }
            Event::RetryRequest(key) => {
                if let Some(pending) = &self.pending
                    && pending.request.key == key
                    && self
                        .power_block_reason(
                            &pending.request.guest,
                            pending.request.action,
                            now,
                            true,
                        )
                        .is_none()
                {
                    self.confirmation = Some(Confirmation::Retry {
                        token: Uuid::new_v4(),
                        request: pending.request.clone(),
                    });
                    self.power_error = None;
                }
            }
            Event::ReviewAcknowledgement(operation_id) => {
                if self.can_acknowledge(operation_id, now) {
                    self.confirmation = Some(Confirmation::Acknowledge {
                        token: Uuid::new_v4(),
                        operation_id,
                    });
                    self.power_error = None;
                }
            }
            Event::Operations {
                session,
                generation,
                revision,
                response,
            } => {
                if session != self.session || generation != self.operations_generation {
                    return Effect::None;
                }
                self.operations_loading = false;
                if revision != self.operations_revision {
                    return Effect::RefreshOperations;
                }
                match response {
                    Ok(operations) => {
                        for operation in operations {
                            self.merge_operation(operation);
                        }
                        self.operations_received_at = Some(now);
                        self.operations_error = None;
                        if let Some(pending) = &mut self.pending
                            && !pending.in_flight
                        {
                            if self
                                .operations
                                .iter()
                                .any(|operation| pending.request.matches(operation))
                            {
                                self.pending = None;
                                self.confirmation = None;
                            } else {
                                pending.checked_missing = pending.error.is_some();
                            }
                        }
                    }
                    Err(error) => {
                        self.operations_error = Some(error);
                        if let Some(pending) = &mut self.pending {
                            pending.checked_missing = false;
                        }
                    }
                }
            }
            Event::Rejected(session, key, message) => {
                if session != self.session
                    || !self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.in_flight && pending.request.key == key)
                {
                    return Effect::None;
                }
                self.pending = None;
                self.confirmation = None;
                self.power_error = Some(format!("Power request rejected: {message}"));
                self.operations_received_at = None;
                self.operations_revision = self.operations_revision.saturating_add(1);
                return Effect::RefreshOperations;
            }
            Event::Submitted(session, key, response) => {
                if session != self.session
                    || !self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.in_flight && pending.request.key == key)
                {
                    return Effect::None;
                }
                let pending = self.pending.as_mut().unwrap();
                match response {
                    Ok(operation) if pending.request.matches(&operation) => {
                        self.pending = None;
                        self.merge_operation(operation);
                        self.received_at = None;
                    }
                    response => {
                        pending.in_flight = false;
                        pending.checked_missing = false;
                        pending.error = Some(match response {
                            Err(error) => error,
                            Ok(_) => "The reply did not match the original power request.".into(),
                        });
                        self.operations_received_at = None;
                    }
                }
                self.operations_revision = self.operations_revision.saturating_add(1);
                return Effect::RefreshOperations;
            }
            Event::Acknowledged(session, operation_id, response) => {
                if session != self.session || self.acknowledging != Some(operation_id) {
                    return Effect::None;
                }
                self.acknowledging = None;
                match response {
                    Ok(operation)
                        if operation.id == operation_id
                            && operation.phase == VirtualizationOperationPhase::Acknowledged
                            && self.operations.iter().any(|existing| {
                                existing.id == operation_id
                                    && existing.guest == operation.guest
                                    && existing.action == operation.action
                            }) =>
                    {
                        self.merge_operation(operation);
                        self.received_at = None;
                    }
                    response => {
                        self.power_error = Some(format!(
                            "Safety hold release is not confirmed: {}. Check operations; no power action was retried.",
                            match response {
                                Err(error) => error,
                                Ok(_) => "Unexpected acknowledgement reply".into(),
                            }
                        ));
                        self.operations_received_at = None;
                    }
                }
                self.operations_revision = self.operations_revision.saturating_add(1);
                return Effect::RefreshOperations;
            }
        }
        Effect::None
    }

    pub fn begin_operations_refresh(&mut self) -> (Uuid, u64, u64) {
        self.operations_loading = true;
        self.operations_generation = self.operations_generation.saturating_add(1);
        (
            self.session,
            self.operations_generation,
            self.operations_revision,
        )
    }

    pub fn reconnect(&mut self) {
        self.session = Uuid::new_v4();
        self.received_at = None;
        self.operations_received_at = None;
        self.operations_loading = false;
        self.inventory_loading = false;
        self.acknowledging = None;
        self.confirmation = None;
        if let Some(pending) = &mut self.pending {
            pending.in_flight = false;
            pending.checked_missing = false;
            pending.error = Some("Connection replaced before the request was resolved. Check the server using the original request key.".into());
        }
    }

    fn merge_operation(&mut self, operation: VirtualizationOperation) {
        if let Some(existing) = self
            .operations
            .iter_mut()
            .find(|existing| existing.id == operation.id)
        {
            if existing.updated_at > operation.updated_at
                || phase_rank(existing.phase) > phase_rank(operation.phase)
            {
                return;
            }
            if existing.phase.blocks_guest() && !operation.phase.blocks_guest() {
                self.received_at = None;
            }
            *existing = operation;
        } else {
            self.operations.push(operation);
        }
        self.operations.sort_by_key(|operation| {
            (
                !operation.phase.blocks_guest(),
                std::cmp::Reverse(operation.updated_at),
            )
        });
        self.operations.truncate(100);
    }

    pub fn operations_stale(&self, now: u64) -> bool {
        self.operations_error.is_some() || observation_stale(self.operations_received_at, now)
    }

    pub fn power_block_reason(
        &self,
        identity: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
        now: u64,
        recovering: bool,
    ) -> Option<&'static str> {
        if identity.kind != VirtualizationGuestKind::VirtualMachine {
            return Some("Power controls support QEMU VMs only, not containers.");
        }
        let Some(guest) = self.inventory.as_ref().and_then(|inventory| {
            inventory.guests.iter().find(|guest| {
                guest.node == identity.node
                    && guest.vmid == identity.vmid
                    && guest.kind == identity.kind
            })
        }) else {
            return Some("This guest is not in the current visible inventory.");
        };
        if guest.template {
            return Some("Templates do not have power controls.");
        }
        if self.stale(now) {
            return Some("Power controls are paused until inventory is fresh.");
        }
        if self.operations_stale(now) {
            return Some("Power controls are paused until the operations list is fresh.");
        }
        if self.acknowledging.is_some() {
            return Some("Wait for safety hold acknowledgement to finish.");
        }
        if let Some(pending) = &self.pending {
            if pending.in_flight {
                return Some("A power request is being submitted. Do not submit again.");
            }
            if !recovering || pending.request.guest != *identity || pending.request.action != action
            {
                return Some("Resolve the uncertain power request before starting another action.");
            }
            if !pending.checked_missing || self.operations_loading {
                return Some("Check operations for the original request before a same-key retry.");
            }
        } else if recovering {
            return Some("There is no unresolved request to retry.");
        }
        if self.operations.iter().any(|operation| {
            operation.guest.node == identity.node
                && operation.guest.vmid == identity.vmid
                && operation.phase.blocks_guest()
        }) {
            return Some("This guest has an active operation or an Unknown safety hold.");
        }
        if !self
            .inventory
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .any(|node| node.name == identity.node && node.status == "online")
        {
            return Some("The guest's node is not reported online.");
        }
        let expected = match action {
            VirtualizationPowerAction::Start => "stopped",
            VirtualizationPowerAction::Shutdown | VirtualizationPowerAction::ForceStop => "running",
        };
        if guest.status != expected {
            return Some("The guest's reported state no longer allows this action.");
        }
        None
    }

    pub fn can_acknowledge(&self, operation_id: Uuid, now: u64) -> bool {
        !self.operations_stale(now)
            && self.acknowledging.is_none()
            && self.pending.is_none()
            && self.operations.iter().any(|operation| {
                operation.id == operation_id
                    && operation.phase == VirtualizationOperationPhase::Unknown
            })
    }

    pub fn receive(&mut self, response: Result<VirtualizationInventory, String>, now: u64) {
        match response {
            Ok(inventory) => {
                self.inventory = Some(inventory);
                self.received_at = Some(now);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn stale(&self, now: u64) -> bool {
        self.error.is_some() || observation_stale(self.received_at, now)
    }
}

impl PowerRequest {
    fn matches(&self, operation: &VirtualizationOperation) -> bool {
        self.key == operation.id && self.guest == operation.guest && self.action == operation.action
    }
}

fn observation_stale(received_at: Option<u64>, now: u64) -> bool {
    received_at.is_none_or(|received| received > now || now.saturating_sub(received) > 30)
}

fn phase_rank(phase: VirtualizationOperationPhase) -> u8 {
    match phase {
        VirtualizationOperationPhase::Submitting => 0,
        VirtualizationOperationPhase::Running => 1,
        VirtualizationOperationPhase::Unknown => 2,
        VirtualizationOperationPhase::Succeeded | VirtualizationOperationPhase::Failed => 3,
        VirtualizationOperationPhase::Acknowledged => 4,
    }
}

impl crate::App {
    pub fn refresh_cockpit_operations(&mut self) -> iced::Task<crate::Msg> {
        let (Some(client), Some(cockpit)) = (self.client.clone(), &mut self.cockpit) else {
            return iced::Task::none();
        };
        let (session, generation, revision) = cockpit.begin_operations_refresh();
        self.refreshing = true;
        iced::Task::perform(
            async move { crate::result(client.virtualization_operations().await) },
            move |response| {
                crate::Msg::Cockpit(Event::Operations {
                    session,
                    generation,
                    revision,
                    response,
                })
            },
        )
    }

    pub fn update_cockpit(&mut self, event: Event) -> iced::Task<crate::Msg> {
        let Some(cockpit) = &mut self.cockpit else {
            return iced::Task::none();
        };
        let navigate = matches!(
            event,
            Event::Navigate(_)
                | Event::Inspect(_)
                | Event::ReviewPower(_, _)
                | Event::ReviewAcknowledgement(_)
                | Event::RetryRequest(_)
        );
        if matches!(event, Event::Confirm(_) | Event::RetryRequest(_)) && self.client.is_none() {
            cockpit.power_error = Some("No authenticated server connection is available.".into());
            return iced::Task::none();
        }
        let effect = cockpit.update(event);
        self.refreshing = cockpit.inventory_loading || cockpit.operations_loading;
        let session = cockpit.session;
        let task = match effect {
            Effect::None => iced::Task::none(),
            Effect::RefreshOperations => self.refresh_cockpit_operations(),
            Effect::Submit(request) => {
                let client = self.client.clone().unwrap();
                let key = request.key;
                iced::Task::perform(
                    async move {
                        Event::submitted(
                            session,
                            key,
                            client
                                .rpc_response(Request::VirtualizationPower {
                                    idempotency_key: key,
                                    guest: request.guest,
                                    action: request.action,
                                })
                                .await,
                        )
                    },
                    crate::Msg::Cockpit,
                )
            }
            Effect::Acknowledge(operation_id) => {
                let client = self.client.clone().unwrap();
                iced::Task::perform(
                    async move {
                        crate::result(
                            client
                                .acknowledge_virtualization_operation(operation_id)
                                .await,
                        )
                    },
                    move |response| {
                        crate::Msg::Cockpit(Event::Acknowledged(session, operation_id, response))
                    },
                )
            }
        };
        if navigate {
            task.chain(iced::widget::operation::scroll_to(
                "cockpit-body",
                iced::widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
            ))
        } else {
            task
        }
    }
}
