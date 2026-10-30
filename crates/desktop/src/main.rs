use homecloud_client::{Client, default_profile};
use homecloud_core::*;
use iced::{Subscription, Task};
use iced_m3::{Element, Theme, text_field};
mod allocation;
mod create_recovery;
#[cfg(test)]
mod create_recovery_tests;
#[cfg(test)]
mod dialog_tests;
#[cfg(test)]
mod fab_tests;
#[cfg(test)]
mod host_ui_tests;
#[cfg(test)]
mod memory_ui_tests;
mod preferences;
#[cfg(test)]
mod preferences_tests;
#[cfg(test)]
mod reserve_ui_tests;
mod reserves;
mod resources;
mod ui;
#[cfg(test)]
mod ui_status_tests;
#[cfg(test)]
mod visual_tests;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Workspaces,
    Workspace(Uuid),
    Hosts,
    Host,
    People,
    Activity,
    Settings,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HostTab {
    #[default]
    Overview,
    Allocations,
    Hardware,
}
#[derive(Clone)]
enum Panel {
    Create,
    SetupDiagnostics(Uuid),
    HostReserves,
    ReviewMemoryCapacity,
    EditWorkspace(Uuid),
    ConfirmEdit { vm_id: Uuid, changes: WorkspaceEdit },
    Invite,
    Grant,
    Invitation,
    Delete(Uuid),
    Stop(Uuid),
    ForceStop(Uuid),
    Revoke(String),
    GpuUnavailable(GpuUnavailable),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Ticket,
    Token,
    Name,
    Ram,
    Cores,
    Disk,
    Key,
    User,
    RamGrant,
    CoreGrant,
    DiskGrant,
    ReserveMemory,
    ReserveCores,
    ReserveDisk,
    ArcLimit,
}
const FIELD_COUNT: usize = Field::ArcLimit as usize + 1;
#[derive(Clone)]
enum Msg {
    Edit(Field, String),
    ResourceValue(Field, u64),
    SaveHostReserves,
    AdoptMemoryCapacity,
    RetryMemoryCapacity(MemoryCapacityOperation),
    CancelMemoryCapacity(Uuid),
    ReserveAutomatic(bool),
    RetryHostReserves(Uuid),
    DiscardHostReserves(Uuid),
    Navigate(Page),
    ReviewHostMemory,
    Open(Panel),
    OpenWorkspace(Uuid),
    ToggleAdvanced,
    ToggleAppDetails,
    AuthorizationInput(String),
    SubmitAuthorization(Uuid, Uuid),
    CopyAuthorization(Uuid),
    Close,
    DismissNotice,
    ToggleHardware,
    HostTab(HostTab),
    HighlightOwner(Option<Uuid>),
    ToggleIdentity,
    ToggleMotion,
    Retry(Uuid),
    LoadSetupDiagnostics(Uuid),
    SetupDiagnosticsLoaded(Uuid, Result<VmDiagnostics, String>),
    Pair,
    Loaded(Result<Arc<Client>, String>),
    Refresh,
    Snapshot(Result<Box<Snapshot>, String>),
    Reply(Result<Response, String>),
    Create,
    CreateReply(Uuid, Result<Response, String>),
    Change(Mutation),
    StartWithoutGpu(Uuid),
    Delete(Uuid),
    ForceStop(Uuid),
    Stop(Uuid),
    SaveWorkspace(Uuid),
    ConfirmWorkspaceEdit(Uuid),
    BackToEdit(Uuid),
    EditGpu(GpuEdit),
    ToggleEditGpu(String, bool),
    Select(Uuid),
    Grant,
    Invite,
    ToggleGpu,
    ToggleT3,
    ToggleManagedHost,
    SelectWorkload(Option<ApplicationKind>),
    InstallManagedHost(Uuid),
    InstallApplication(Uuid),
    SetupApplication(Uuid, ApplicationSetupAction),
    OpenAuthorization(Uuid),
    BrowserOpened(Uuid, Result<(), String>),
    Revoke(String),
    CopyInvite,
    Ssh(Uuid, bool),
    Exported(Result<(String, Option<String>), String>),
}
// Avoid formatting invitation codes or credentials in diagnostics.
impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Homecloud event")
    }
}
#[derive(Clone)]
struct ApplicationRequest {
    id: Uuid,
    target: Option<ApplicationSetupAction>,
    submitting_code: bool,
}
struct App {
    profile: PathBuf,
    preferences_path: Option<PathBuf>,
    client: Option<Arc<Client>>,
    snapshot: Option<Snapshot>,
    fields: [String; FIELD_COUNT],
    resource_bounds: Option<resources::FormBounds>,
    reserve_draft: Option<reserves::ReserveDraft>,
    memory_review: Option<MemoryCapacityStatus>,
    accepted_reserve: Option<HostReserveOperation>,
    error: Option<String>,
    notice: String,
    busy: bool,
    refreshing: bool,
    snapshot_unavailable: bool,
    vm_request: Option<(Uuid, Mutation)>,
    create_outbox: Option<create_recovery::Outbox>,
    create_pending: Option<create_recovery::PendingCreate>,
    last_create: Option<create_recovery::PendingCreate>,
    create_inflight: Option<Uuid>,
    create_error: Option<String>,
    accepted_vm_ops: Vec<Operation>,
    setup_diagnostics: Option<(Uuid, Result<VmDiagnostics, String>)>,
    setup_diagnostics_loading: bool,
    selected: Option<Uuid>,
    gpu: bool,
    install_t3: bool,
    install_managed_host: bool,
    edit_gpu: GpuEdit,
    invite: Option<String>,
    page: Page,
    panel: Option<Panel>,
    last_panel: Option<Panel>,
    last_panel_error: Option<String>,
    hardware_details: bool,
    host_tab: HostTab,
    advanced: bool,
    host_owner: Option<Uuid>,
    identity_details: bool,
    reduced_motion: bool,
    app_details: bool,
    app_request: Option<ApplicationRequest>,
    accepted_app_ops: Vec<ApplicationOperation>,
    authorization_input: String,
    browser_opened: Option<Uuid>,
    submitted_code: Option<Uuid>,
}
fn result<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}
impl App {
    fn blank(profile: PathBuf) -> Self {
        let mut fields: [String; FIELD_COUNT] = Default::default();
        for (field, value) in [
            (Field::Name, "devbox"),
            (Field::Ram, "2"),
            (Field::Cores, "1"),
            (Field::Disk, "20"),
            (Field::RamGrant, "2"),
            (Field::CoreGrant, "1"),
            (Field::DiskGrant, "32"),
        ] {
            fields[field as usize] = value.into();
        }
        Self {
            profile,
            // Fixtures never resolve or access the real user's preferences.
            preferences_path: None,
            client: None,
            snapshot: None,
            fields,
            resource_bounds: None,
            reserve_draft: None,
            memory_review: None,
            accepted_reserve: None,
            error: None,
            notice: String::new(),
            busy: false,
            refreshing: false,
            snapshot_unavailable: false,
            vm_request: None,
            create_outbox: None,
            create_pending: None,
            last_create: None,
            create_inflight: None,
            create_error: None,
            accepted_vm_ops: vec![],
            setup_diagnostics: None,
            setup_diagnostics_loading: false,
            selected: None,
            gpu: false,
            install_t3: true,
            install_managed_host: false,
            edit_gpu: GpuEdit::Keep,
            invite: None,
            page: Page::Workspaces,
            panel: None,
            last_panel: None,
            last_panel_error: None,
            hardware_details: false,
            host_tab: HostTab::Overview,
            advanced: false,
            host_owner: None,
            identity_details: false,
            reduced_motion: false,
            app_details: false,
            app_request: None,
            accepted_app_ops: vec![],
            authorization_input: String::new(),
            browser_opened: None,
            submitted_code: None,
        }
    }
    fn new() -> (Self, Task<Msg>) {
        let profile = std::env::var_os("HOMECLOUD_PROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(default_profile);
        let task = if profile.exists() {
            let p = profile.clone();
            Task::perform(async move { result(Client::load(&p).await) }, Msg::Loaded)
        } else {
            Task::none()
        };
        let mut app = Self::blank(profile);
        match preferences::default_path() {
            Ok(path) => {
                app.preferences_path = Some(path);
                app.load_preferences();
            }
            Err(error) => app.error = Some(format!("Could not load preferences: {error}")),
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        app.fields[Field::Key as usize] = std::fs::read_to_string(home.join(".ssh/id_ed25519.pub"))
            .unwrap_or_default()
            .trim()
            .into();
        (app, task)
    }
    fn load_preferences(&mut self) {
        let Some(path) = self.preferences_path.as_deref() else {
            self.error =
                Some("Could not load preferences: Configuration directory unavailable".into());
            return;
        };
        match preferences::Preferences::load(path, &self.profile) {
            Ok(loaded) => {
                // Honor the imported setting even if persistence fails, and
                // retain its source file so a later launch can retry the import.
                self.reduced_motion = loaded.preferences.reduced_motion;
                if loaded.needs_save
                    && let Err(error) = loaded.preferences.save(path)
                {
                    self.error = Some(format!("Could not save preferences: {error}"));
                }
            }
            Err(error) => self.error = Some(format!("Could not load preferences: {error}")),
        }
    }
    fn theme(&self) -> Theme {
        Theme::from_accent(iced::Color::from_rgb8(101, 151, 235), true)
            .expressive()
            .reduced_motion(self.reduced_motion)
    }
    fn close_panel(&mut self) {
        if let Some(panel) = self.panel.take() {
            self.last_panel_error = self.error.clone();
            self.last_panel = Some(panel);
        }
        // The modal still renders this form during its exit transition. Keep
        // its ranges until a new form replaces them; active handlers use panel.
    }
    fn get(&self, f: Field) -> &str {
        &self.fields[f as usize]
    }
    fn input(&self, label: &'static str, f: Field) -> Element<'_, Msg> {
        text_field(label, self.get(f))
            .on_input(move |s| Msg::Edit(f, s))
            .into()
    }
    fn rpc(&mut self, r: Request) -> Task<Msg> {
        let Some(c) = self.client.clone() else {
            return Task::none();
        };
        self.busy = true;
        self.error = None;
        Task::perform(async move { result(c.rpc(r).await) }, Msg::Reply)
    }
    fn load_create_outbox(&mut self, binding: create_recovery::Binding) -> anyhow::Result<()> {
        self.create_outbox = None;
        self.create_inflight = None;
        self.create_pending = None;
        self.last_create = None;
        let outbox = create_recovery::Outbox::open(&self.profile, binding)?;
        self.create_pending = outbox.load()?;
        self.create_outbox = Some(outbox);
        self.create_error = None;
        if self.create_pending.is_some() {
            self.panel = Some(Panel::Create);
        }
        Ok(())
    }
    fn displayed_create(&self) -> Option<&create_recovery::PendingCreate> {
        self.create_pending.as_ref().or_else(|| {
            if self.panel.is_none() && matches!(self.last_panel, Some(Panel::Create)) {
                self.last_create.as_ref()
            } else {
                None
            }
        })
    }
    fn submit_saved_create(&mut self) -> Task<Msg> {
        if self.busy
            || self.create_inflight.is_some()
            || self.create_error.is_some()
            || self.create_outbox.is_none()
        {
            return Task::none();
        }
        let (Some(client), Some(pending)) = (self.client.clone(), self.create_pending.clone())
        else {
            return Task::none();
        };
        self.busy = true;
        self.create_inflight = Some(pending.key);
        self.error = None;
        let key = pending.key;
        Task::perform(
            async move { result(client.rpc_response(pending.request()).await) },
            move |reply| Msg::CreateReply(key, reply),
        )
    }
    fn begin_create(&mut self, spec: VmSpec) -> Task<Msg> {
        if self.client.is_none() || self.busy || self.create_inflight.is_some() {
            return Task::none();
        }
        if self.create_pending.is_some() {
            self.error =
                Some("Resolve the saved Create request before submitting another VM.".into());
            return Task::none();
        }
        if let Some(outbox) = &self.create_outbox {
            match outbox.load() {
                Ok(Some(pending)) => {
                    self.create_pending = Some(pending);
                    self.panel = Some(Panel::Create);
                    self.error = Some("A saved Create request needs confirmation. Review it and retry to check its result.".into());
                    return Task::none();
                }
                Ok(None) => {}
                Err(error) => {
                    self.create_error = Some(error.to_string());
                    self.error = self.create_error.clone();
                    return Task::none();
                }
            }
        }
        let prepared = self
            .create_outbox
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(self.create_error.clone().unwrap_or_else(|| {
                    "Create recovery storage is unavailable; reopen Homecloud.".into()
                }))
            })
            .and_then(|outbox| outbox.prepare(spec));
        match prepared {
            Ok(pending) => {
                self.create_pending = Some(pending);
                self.submit_saved_create()
            }
            Err(error) => {
                // A rename can have succeeded even if directory sync failed.
                // Recover that exact record before allowing another request.
                if let Some(outbox) = &self.create_outbox {
                    match outbox.load() {
                        Ok(pending) => self.create_pending = pending,
                        Err(error) => self.create_error = Some(error.to_string()),
                    }
                }
                self.error = Some(error.to_string());
                Task::none()
            }
        }
    }
    fn resolve_create(&mut self, key: Uuid) -> anyhow::Result<()> {
        self.create_outbox
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Create recovery storage is unavailable"))?
            .resolve(key)?;
        self.last_create = self.create_pending.take();
        Ok(())
    }
    fn create_reply(&mut self, key: Uuid, reply: Result<Response, String>) -> Task<Msg> {
        if self.create_inflight != Some(key)
            || self.create_pending.as_ref().is_none_or(|p| p.key != key)
        {
            return Task::none();
        }
        self.create_inflight = None;
        self.busy = false;
        match reply {
            Ok(Response::Operation(op)) if self.create_pending.as_ref().is_some_and(|p| p.matches(&op.request)) => {
                self.accepted_vm_ops.retain(|old| old.id != op.id);
                self.accepted_vm_ops.push(op.clone());
                if let Some(snapshot) = &mut self.snapshot {
                    snapshot.operations.retain(|old| old.id != op.id);
                    snapshot.operations.insert(0, op);
                }
                match self.resolve_create(key) {
                    Ok(()) => {
                        if matches!(self.panel, Some(Panel::Create)) { self.close_panel(); }
                        self.error = None;
                        self.notice = "VM creation confirmed. Follow its progress in Activity.".into();
                    }
                    Err(error) => self.error = Some(format!("Creation was accepted, but its local receipt could not be cleared: {error}. Retry the saved request to finish recovery.")),
                }
            }
            // This receipt is committed before the server replies and fences
            // every delayed copy of this key. Generic errors remain uncertain.
            Ok(Response::CreateRejected { idempotency_key, message }) if idempotency_key == key => {
                let spec = self.create_pending.as_ref().unwrap().spec.clone();
                match self.resolve_create(key) {
                    Ok(()) => {
                        if matches!(self.panel, Some(Panel::Create)) {
                            self.fields[Field::Name as usize] = spec.name;
                            self.fields[Field::Ram as usize] = spec.memory_gib.to_string();
                            self.fields[Field::Cores as usize] = spec.cores.to_string();
                            self.fields[Field::Disk as usize] = spec.disk_gib.to_string();
                            self.fields[Field::Key as usize] = spec.ssh_public_key;
                            self.install_t3 = spec.applications.contains(&ApplicationKind::T3);
                            self.install_managed_host = spec.applications.contains(&ApplicationKind::HomecloudHost);
                        }
                        self.last_create = None;
                        self.error = Some(message);
                    }
                    Err(error) => self.error = Some(format!("The request was rejected, but its local receipt could not be cleared: {error}. The saved request has been retained.")),
                }
            }
            Ok(Response::Error { message }) | Err(message) => {
                self.error = Some(format!("Could not confirm VM creation: {message}. Your original request is saved; retry it to check the result."));
            }
            Ok(_) => self.error = Some("Unexpected creation response. Your original request is saved; retry it to check the result.".into()),
        }
        self.update(Msg::Refresh)
    }
    fn change(&mut self, mutation: Mutation) -> Task<Msg> {
        if self.busy || self.create_inflight.is_some() || self.snapshot_unavailable {
            return Task::none();
        }
        if let Mutation::Create { spec } = mutation {
            return self.begin_create(spec);
        }
        self.vm_request = match &mutation {
            Mutation::Create { .. } => None,
            Mutation::State { vm_id, .. }
            | Mutation::Start { vm_id, .. }
            | Mutation::ForceStop { vm_id }
            | Mutation::Resize { vm_id, .. }
            | Mutation::Edit { vm_id, .. }
            | Mutation::Gpu { vm_id, .. } => Some((*vm_id, mutation.clone())),
        };
        self.rpc(Request::Mutate {
            idempotency_key: Uuid::new_v4(),
            mutation,
        })
    }
    fn sizes(&self) -> anyhow::Result<(u64, u64, u64)> {
        Ok((
            self.get(Field::Ram).parse()?,
            self.get(Field::Cores).parse()?,
            self.get(Field::Disk).parse()?,
        ))
    }
    fn workspace_edit(&self) -> anyhow::Result<WorkspaceEdit> {
        let (memory_gib, cores, disk_gib) = self
            .sizes()
            .map_err(|_| anyhow::anyhow!("Enter whole numbers for memory, cores and disk."))?;
        let name = self.get(Field::Name).trim().to_owned();
        anyhow::ensure!(
            !name.is_empty() && name.len() <= 80 && !name.chars().any(char::is_control),
            "Enter a VM name of 1–80 characters."
        );
        anyhow::ensure!(
            (1..=1024).contains(&memory_gib)
                && (1..=256).contains(&cores)
                && (10..=65536).contains(&disk_gib),
            "Use at least 1 GiB RAM, one core and 10 GiB disk."
        );
        if let Some(bounds) = resources::FormBounds::current(self) {
            bounds.validate(self).map_err(anyhow::Error::msg)?;
        }
        Ok(WorkspaceEdit {
            name,
            memory_gib,
            cores,
            disk_gib,
            gpu: self.edit_gpu.clone(),
        })
    }
    fn grant(&self) -> anyhow::Result<Resources> {
        if let Some(bounds) = resources::FormBounds::current(self) {
            bounds.validate(self).map_err(anyhow::Error::msg)?;
        }
        Ok(Resources {
            memory_bytes: self
                .get(Field::RamGrant)
                .parse::<u64>()?
                .checked_mul(GIB)
                .ok_or_else(|| anyhow::anyhow!("RAM overflow"))?,
            cpu_cores: self.get(Field::CoreGrant).parse()?,
            disk_bytes: self
                .get(Field::DiskGrant)
                .parse::<u64>()?
                .checked_mul(GIB)
                .ok_or_else(|| anyhow::anyhow!("Disk overflow"))?,
        })
    }
    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::ReviewHostMemory => {
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.me.role == Role::Admin)
                {
                    self.page = Page::Host;
                    self.host_tab = HostTab::Overview;
                    self.host_owner = None;
                    self.close_panel();
                    self.error = None;
                    return iced::widget::operation::scroll_to(
                        "page-body",
                        iced::widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
                    );
                }
            }
            Msg::Navigate(page) => {
                if page == Page::Host && self.page != Page::Host {
                    self.host_tab = HostTab::Overview;
                    self.host_owner = None;
                    self.hardware_details = false;
                }
                self.authorization_input.clear();
                self.page = page;
                self.close_panel();
                self.error = None;
            }
            Msg::Open(panel) => {
                self.error = None;
                if matches!(panel, Panel::Create) && self.create_pending.is_some() {
                    self.panel = Some(panel);
                    return Task::none();
                }
                if matches!(panel, Panel::Create) {
                    self.last_create = None;
                }
                if matches!(panel, Panel::ReviewMemoryCapacity) {
                    self.memory_review = self
                        .snapshot
                        .as_ref()
                        .filter(|s| s.me.role == Role::Admin)
                        .and_then(|s| s.host_reserves.as_ref())
                        .and_then(|v| v.memory.clone());
                }
                if matches!(panel, Panel::HostReserves) {
                    match reserves::ReserveDraft::capture(self) {
                        Ok(draft) => {
                            for (field, value) in reserves::FIELDS.into_iter().zip([
                                draft.current.memory_bytes / GIB,
                                draft.current.cpu_cores,
                                draft.current.disk_bytes / GIB,
                            ]) {
                                self.fields[field as usize] = value.to_string();
                            }
                            let limit = match draft.current_arc {
                                Some(ArcMode::Custom { max_bytes }) => max_bytes / GIB,
                                _ => self
                                    .snapshot
                                    .as_ref()
                                    .and_then(|s| s.host_reserves.as_ref())
                                    .and_then(|v| v.arc.as_ref())
                                    .map(|a| a.maximum_bytes / GIB)
                                    .unwrap_or(2),
                            };
                            self.fields[Field::ArcLimit as usize] =
                                limit.max(draft.arc_bounds.min).to_string();
                            self.reserve_draft = Some(draft);
                        }
                        Err(error) => {
                            self.error = Some(error.to_string());
                            return Task::none();
                        }
                    }
                }
                if let Panel::EditWorkspace(id) = panel
                    && let Some(vm) = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.vms.iter().find(|v| v.id == id))
                {
                    self.fields[Field::Name as usize] = vm.spec.name.clone();
                    self.edit_gpu = GpuEdit::Keep;
                    self.fields[Field::Ram as usize] = vm.spec.memory_gib.to_string();
                    self.fields[Field::Cores as usize] = vm.spec.cores.to_string();
                    self.fields[Field::Disk as usize] = vm.spec.disk_gib.to_string();
                }
                if matches!(panel, Panel::Invite) {
                    self.selected = None;
                    self.fields[Field::User as usize] = String::new();
                    self.gpu = false;
                }
                self.panel = Some(panel);
                self.resource_bounds = resources::FormBounds::capture(self);
                if matches!(self.panel, Some(Panel::Create | Panel::Invite))
                    && let Some(bounds) = self.resource_bounds
                {
                    let defaults = if matches!(self.panel, Some(Panel::Create)) {
                        [(Field::Ram, 2), (Field::Cores, 1), (Field::Disk, 20)]
                    } else {
                        [
                            (Field::RamGrant, 2),
                            (Field::CoreGrant, 1),
                            (Field::DiskGrant, 32),
                        ]
                    };
                    for (field, value) in defaults {
                        let bound = bounds.for_field(field);
                        self.fields[field as usize] =
                            value.clamp(bound.min, bound.max.max(bound.min)).to_string();
                    }
                }
            }
            Msg::OpenWorkspace(id) => {
                self.page = Page::Workspace(id);
                self.advanced = false;
                self.identity_details = false;
                self.panel = None;
                self.last_panel = None;
                self.last_panel_error = None;
                self.error = None;
                self.app_details = false;
                self.authorization_input.clear();
                return iced::widget::operation::scroll_to(
                    "page-body",
                    iced::widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
                );
            }
            Msg::ToggleAdvanced => self.advanced = !self.advanced,
            Msg::ToggleAppDetails => self.app_details = !self.app_details,
            Msg::AuthorizationInput(value) => self.authorization_input = value,
            Msg::CopyAuthorization(id) => {
                if let Some(action) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.applications.iter().find(|a| a.id == id))
                    .and_then(|a| a.observation.action.as_ref())
                    .filter(|a| a.expires_at > unix_now() && !a.code.is_empty())
                {
                    return iced::clipboard::write(action.code.clone());
                }
            }
            Msg::SubmitAuthorization(application_id, operation_id) => {
                if self.app_request.is_some() {
                    return Task::none();
                }
                let code = AuthorizationCode(
                    std::mem::take(&mut self.authorization_input)
                        .trim()
                        .to_owned(),
                );
                self.app_request = Some(ApplicationRequest {
                    id: application_id,
                    target: Some(ApplicationSetupAction::SignInClaude),
                    submitting_code: true,
                });
                self.submitted_code = Some(operation_id);
                return self.rpc(Request::SubmitApplicationCode {
                    application_id,
                    operation_id,
                    code,
                });
            }
            Msg::Close => {
                if !self.busy {
                    self.close_panel();
                    self.error = None;
                }
            }
            Msg::DismissNotice => {
                self.error = None;
                self.notice.clear();
            }
            Msg::ToggleIdentity => self.identity_details = !self.identity_details,
            Msg::ToggleHardware => self.hardware_details = !self.hardware_details,
            Msg::HostTab(tab) => self.host_tab = tab,
            Msg::HighlightOwner(owner) => self.host_owner = owner,
            Msg::ToggleMotion => {
                let Some(path) = self.preferences_path.as_deref() else {
                    self.error = Some(
                        "Could not save preferences: Configuration directory unavailable".into(),
                    );
                    return Task::none();
                };
                let preferences = preferences::Preferences {
                    reduced_motion: !self.reduced_motion,
                };
                match preferences.save(path) {
                    Ok(()) => {
                        self.reduced_motion = preferences.reduced_motion;
                        if self.error.as_deref().is_some_and(|error| {
                            error.starts_with("Could not save preferences:")
                                || error.starts_with("Could not load preferences:")
                        }) {
                            self.error = None;
                        }
                    }
                    Err(error) => {
                        self.error = Some(format!("Could not save preferences: {error}"));
                    }
                }
            }
            Msg::LoadSetupDiagnostics(id) => {
                if self.setup_diagnostics_loading {
                    return Task::none();
                }
                let Some(client) = self.client.clone() else {
                    return Task::none();
                };
                self.panel = Some(Panel::SetupDiagnostics(id));
                self.setup_diagnostics_loading = true;
                self.setup_diagnostics = None;
                return Task::perform(
                    async move {
                        match client.rpc(Request::VmDiagnostics { vm_id: id }).await {
                            Ok(Response::VmDiagnostics(report)) => Ok(report),
                            Ok(Response::Error { message }) => Err(message),
                            Ok(_) => Err("Unexpected setup diagnostics response".into()),
                            Err(error) => Err(error.to_string()),
                        }
                    },
                    move |report| Msg::SetupDiagnosticsLoaded(id, report),
                );
            }
            Msg::SetupDiagnosticsLoaded(id, report) => {
                self.setup_diagnostics_loading = false;
                // A late response never replaces another VM's content or
                // changes a closing dialog.
                if matches!(self.panel.as_ref(), Some(Panel::SetupDiagnostics(current)) if *current == id)
                {
                    self.setup_diagnostics = Some((id, report));
                }
            }
            Msg::Retry(operation_id) => return self.rpc(Request::RetryOperation { operation_id }),
            Msg::Edit(f, s) => {
                if !(matches!(self.panel, Some(Panel::Create)) && self.create_pending.is_some()) {
                    self.fields[f as usize] = s;
                }
            }
            Msg::ResourceValue(field, value) => {
                if matches!(self.panel, Some(Panel::Create)) && self.create_pending.is_some() {
                    return Task::none();
                }
                if reserves::FIELDS.contains(&field) || field == Field::ArcLimit {
                    if !self.busy
                        && !self.snapshot_unavailable
                        && matches!(self.panel, Some(Panel::HostReserves))
                        && self
                            .reserve_draft
                            .as_ref()
                            .and_then(|d| d.bound(field))
                            .is_some_and(|b| b.accepts(value))
                    {
                        self.fields[field as usize] = value.to_string();
                    }
                    return Task::none();
                }
                if !self.busy
                    && !self.snapshot_unavailable
                    && matches!(
                        field,
                        Field::Ram
                            | Field::Cores
                            | Field::Disk
                            | Field::RamGrant
                            | Field::CoreGrant
                            | Field::DiskGrant
                    )
                    && let Some(bounds) = resources::FormBounds::current(self)
                    && bounds.for_field(field).accepts(value)
                {
                    self.fields[field as usize] = value.to_string();
                }
            }
            Msg::ReserveAutomatic(enabled) => {
                if !self.busy
                    && !self.snapshot_unavailable
                    && matches!(self.panel, Some(Panel::HostReserves))
                    && let Some(draft) = &mut self.reserve_draft
                    && (!enabled || draft.automatic_supported)
                {
                    draft.automatic = enabled;
                }
            }
            Msg::RetryMemoryCapacity(op) => {
                return self.rpc(Request::AdoptMemoryCapacity {
                    expected_revision: op.expected_revision,
                    target_bytes: op.target_bytes,
                    idempotency_key: op.id,
                });
            }
            Msg::CancelMemoryCapacity(operation_id) => {
                return self.rpc(Request::CancelMemoryCapacity { operation_id });
            }
            Msg::AdoptMemoryCapacity => {
                if self.busy
                    || self.snapshot_unavailable
                    || !matches!(self.panel, Some(Panel::ReviewMemoryCapacity))
                {
                    return Task::none();
                }
                let live = self
                    .snapshot
                    .as_ref()
                    .filter(|s| s.me.role == Role::Admin)
                    .and_then(|s| s.host_reserves.as_ref())
                    .and_then(|v| v.memory.as_ref());
                if let (Some(review), Some(live)) = (&self.memory_review, live) {
                    if review.revision != live.revision || review.safe_bytes != live.safe_bytes {
                        self.error =
                            Some("Memory capacity changed. Close and review it again.".into());
                    } else {
                        return self.rpc(Request::AdoptMemoryCapacity {
                            expected_revision: review.revision,
                            target_bytes: review.safe_bytes,
                            idempotency_key: Uuid::new_v4(),
                        });
                    }
                }
            }
            Msg::SaveHostReserves => {
                if self.busy
                    || self.snapshot_unavailable
                    || !matches!(self.panel, Some(Panel::HostReserves))
                {
                    return Task::none();
                }
                let Some(draft) = &self.reserve_draft else {
                    return Task::none();
                };
                let revision = draft.revision;
                match draft
                    .values(self)
                    .and_then(|reserves| Ok((reserves, draft.arc(self)?)))
                {
                    Ok((reserves, arc)) => {
                        return self.rpc(Request::SetHostReserves {
                            reserves,
                            arc,
                            expected_revision: revision,
                            idempotency_key: Uuid::new_v4(),
                        });
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            Msg::RetryHostReserves(operation_id) => {
                return self.rpc(Request::RetryHostReserves { operation_id });
            }
            Msg::DiscardHostReserves(operation_id) => {
                self.accepted_reserve = None;
                return self.rpc(Request::DiscardHostReserves { operation_id });
            }
            Msg::Pair => {
                let server = match serde_json::from_str(self.get(Field::Ticket)) {
                    Ok(s) => s,
                    Err(e) => {
                        self.error = Some(format!("Invalid public host ticket: {e}"));
                        return Task::none();
                    }
                };
                let path = self.profile.clone();
                let token = std::mem::take(&mut self.fields[Field::Token as usize]);
                self.busy = true;
                return Task::perform(
                    async move { result(Client::enroll(&path, server, token).await.map(|(c, _)| c)) },
                    Msg::Loaded,
                );
            }
            Msg::Loaded(r) => {
                self.busy = false;
                match r {
                    Ok(c) => {
                        self.notice.clear();
                        let binding = create_recovery::Binding {
                            server: c.server_id(),
                            device: c.device_id(),
                        };
                        self.client = Some(c);
                        match self.load_create_outbox(binding) {
                            Ok(()) if self.create_pending.is_some() => {
                                return self.submit_saved_create();
                            }
                            Ok(()) => {}
                            Err(error) => {
                                let message =
                                    format!("VM creation recovery is unavailable: {error}");
                                self.create_error = Some(message.clone());
                                self.error = Some(message);
                            }
                        }
                        return self.update(Msg::Refresh);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Refresh => {
                if self.refreshing {
                    return Task::none();
                }
                if let Some(c) = self.client.clone() {
                    self.refreshing = true;
                    return Task::perform(
                        async move { result(c.snapshot().await.map(Box::new)) },
                        Msg::Snapshot,
                    );
                }
            }
            Msg::Snapshot(r) => {
                self.refreshing = false;
                match r {
                    Ok(mut s) => {
                        if s.me.role != Role::Admin {
                            self.accepted_reserve = None;
                        }
                        if let Some(accepted) = self.accepted_reserve.clone() {
                            if let Some(view) = &mut s.host_reserves {
                                if view.operation.as_ref().is_some_and(|op| {
                                    op.id == accepted.id && op.phase == ReservePhase::Cancelled
                                }) {
                                    self.accepted_reserve = None;
                                    self.notice = "Host reserve change discarded.".into();
                                } else if view.active.revision >= accepted.target.revision {
                                    self.accepted_reserve = None;
                                    self.notice = "Host reserves updated.".into();
                                } else if view
                                    .operation
                                    .as_ref()
                                    .is_none_or(|op| op.id != accepted.id)
                                {
                                    view.operation = Some(accepted);
                                }
                                if let Some(op) =
                                    view.operation.as_ref().filter(|op| !op.phase.terminal())
                                {
                                    s.capacity = view
                                        .envelope
                                        .checked_sub(
                                            view.active.additional.max(op.target.additional),
                                        )
                                        .unwrap_or_default();
                                }
                            } else if let Some(mut previous) =
                                self.snapshot.as_ref().and_then(|s| s.host_reserves.clone())
                            {
                                s.capacity = previous
                                    .envelope
                                    .checked_sub(
                                        previous.active.additional.max(accepted.target.additional),
                                    )
                                    .unwrap_or_default();
                                previous.operation = Some(accepted);
                                s.host_reserves = Some(previous);
                            }
                        }

                        self.snapshot_unavailable = false;
                        self.accepted_vm_ops
                            .retain(|op| !s.operations.iter().any(|seen| seen.id == op.id));
                        s.operations.extend(self.accepted_vm_ops.iter().cloned());
                        let reveal = s.applications.iter().any(|a| {
                            self.page == Page::Workspace(a.vm_id)
                                && a.observation.action.as_ref().is_some_and(|c| {
                                    c.setup_action != ApplicationSetupAction::ConnectT3
                                        && c.expires_at > unix_now()
                                })
                                && !self.snapshot.as_ref().is_some_and(|old| {
                                    old.applications.iter().any(|previous| {
                                        previous.id == a.id
                                            && previous.observation.operation_id
                                                == a.observation.operation_id
                                            && previous.observation.action.is_some()
                                    })
                                })
                        });
                        // A completed browser flow gets an explicit acknowledgement,
                        // then its account row returns to the compact signed-in state.
                        for op in &s.application_operations {
                            if op.kind == ApplicationOperationKind::Setup
                                && op.phase == ApplicationPhase::Succeeded
                                && self.snapshot.as_ref().is_some_and(|old| {
                                    old.application_operations.iter().any(|previous| {
                                        previous.id == op.id && previous.phase.active()
                                    })
                                })
                                && s.applications.iter().any(|a| {
                                    a.id == op.application_id
                                        && self.page == Page::Workspace(a.vm_id)
                                })
                            {
                                self.notice = match op.setup_action {
                                    ApplicationSetupAction::ConnectT3 => {
                                        "VM connected to your T3 account."
                                    }
                                    ApplicationSetupAction::SignInCodex => {
                                        "Codex sign-in complete. You can use it in T3 Code."
                                    }
                                    ApplicationSetupAction::SignInClaude => {
                                        "Claude sign-in complete. You can use it in T3 Code."
                                    }
                                }
                                .into();
                            }
                        }
                        // Keep accepted operations visible if an older snapshot was
                        // already in flight when the mutation returned.
                        self.accepted_app_ops.retain(|op| {
                            !s.application_operations
                                .iter()
                                .any(|observed| observed.id == op.id)
                        });
                        s.application_operations
                            .extend(self.accepted_app_ops.iter().cloned());
                        if let Some(id) = self.submitted_code {
                            let current = s.applications.iter().any(|a| {
                                a.observation.operation_id == Some(id)
                                    && a.observation.operation_stage == "verifying"
                            });
                            let done = s
                                .application_operations
                                .iter()
                                .any(|op| op.id == id && !op.phase.active());
                            if current || done {
                                self.submitted_code = None;
                            }
                        }
                        if !s.applications.iter().any(|a| {
                            a.observation.action.as_ref().is_some_and(|c| {
                                c.setup_action == ApplicationSetupAction::SignInClaude
                                    && c.input_required
                                    && c.expires_at > unix_now()
                            })
                        }) {
                            self.authorization_input.clear();
                        }
                        if s.me.role != Role::Admin
                            || !s.users.iter().any(|u| Some(u.id) == self.host_owner)
                        {
                            self.host_owner = None;
                        }
                        self.snapshot = Some(*s);
                        if self
                            .error
                            .as_deref()
                            .is_some_and(|s| s.starts_with("Host unavailable:"))
                        {
                            self.error = None;
                        }
                        if reveal {
                            self.notice =
                                "Your sign-in link is ready in the T3 Code section.".into();
                            if !self.advanced {
                                return iced::widget::operation::snap_to_end("page-body");
                            }
                        }
                    }
                    Err(e) => {
                        self.snapshot_unavailable = true;
                        self.error = Some(format!("Host unavailable: {e}"));
                    }
                }
            }
            Msg::Reply(r) => {
                self.busy = false;
                self.vm_request = None;
                let application_request = self.app_request.take();
                match r {
                    Ok(Response::Invitation { token, .. }) => {
                        self.invite = Some(token);
                        self.panel = Some(Panel::Invitation);
                        self.notice.clear();
                    }
                    Ok(Response::ApplicationOperation(op)) => {
                        self.accepted_app_ops.retain(|old| old.id != op.id);
                        self.accepted_app_ops.push(op.clone());
                        if let Some(s) = &mut self.snapshot {
                            s.application_operations.retain(|old| old.id != op.id);
                            s.application_operations.push(op);
                        }
                        self.notice.clear();
                    }
                    Ok(Response::Ok) if application_request.is_some() => {
                        self.notice.clear();
                    }
                    Ok(Response::HostReserveOperation(op)) => {
                        self.accepted_reserve = Some(op.clone());
                        if let Some(s) = &mut self.snapshot
                            && let Some(view) = &mut s.host_reserves
                        {
                            s.capacity = view
                                .envelope
                                .checked_sub(view.active.additional.max(op.target.additional))
                                .unwrap_or_default();
                            view.operation = Some(op);
                        }
                        self.close_panel();
                        self.notice = "Updating host reserves…".into();
                    }
                    Ok(Response::Operation(op)) => {
                        let completed = op.phase == "succeeded";
                        self.accepted_vm_ops.retain(|old| old.id != op.id);
                        self.accepted_vm_ops.push(op.clone());
                        if let Some(s) = &mut self.snapshot {
                            s.operations.retain(|old| old.id != op.id);
                            s.operations.insert(0, op);
                        }
                        if matches!(self.panel, Some(Panel::Delete(_))) {
                            self.page = Page::Workspaces;
                        }
                        self.close_panel();
                        self.notice = if completed {
                            "Changes saved."
                        } else {
                            "Request submitted. Follow its progress in Activity."
                        }
                        .into();
                    }
                    Ok(Response::GpuUnavailable(conflict)) => {
                        self.panel = Some(Panel::GpuUnavailable(conflict));
                        self.notice.clear();
                        self.error = None;
                    }
                    Ok(_) => {
                        self.close_panel();
                        self.notice = "Changes saved.".into();
                    }
                    Err(e) => {
                        self.error = Some(e);
                        if application_request.is_some_and(|r| r.submitting_code) {
                            self.submitted_code = None;
                        }
                    }
                }
                return self.update(Msg::Refresh);
            }
            Msg::CreateReply(key, reply) => return self.create_reply(key, reply),
            Msg::Create => {
                if !matches!(self.panel, Some(Panel::Create))
                    || self.busy
                    || self.create_inflight.is_some()
                {
                    return Task::none();
                }
                if self.create_pending.is_some() {
                    return self.submit_saved_create();
                }
                match resources::FormBounds::current(self)
                    .ok_or_else(|| anyhow::anyhow!("Resource capacity unavailable."))
                    .and_then(|bounds| bounds.validate(self).map_err(anyhow::Error::msg))
                    .and_then(|_| self.sizes())
                {
                    Ok((memory_gib, cores, disk_gib)) => {
                        return self.change(Mutation::Create {
                            spec: VmSpec {
                                applications: if self.install_managed_host {
                                    vec![ApplicationKind::HomecloudHost]
                                } else if self.install_t3 {
                                    vec![ApplicationKind::T3]
                                } else {
                                    vec![]
                                },
                                name: self.get(Field::Name).into(),
                                memory_gib,
                                cores,
                                disk_gib,
                                ssh_public_key: self.get(Field::Key).trim().into(),
                            },
                        });
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Msg::Change(m) => {
                return self.change(m);
            }
            Msg::StartWithoutGpu(vm_id) => {
                if matches!(&self.panel, Some(Panel::GpuUnavailable(c)) if c.vm_id == vm_id)
                    && !self.busy
                {
                    return self.change(Mutation::Start {
                        vm_id,
                        without_gpu: true,
                    });
                }
            }
            Msg::Delete(id) => {
                if matches!(self.panel,Some(Panel::Delete(current)) if current==id) {
                    return self.change(Mutation::State {
                        vm_id: id,
                        desired: Lifecycle::Deleted,
                    });
                }
                self.panel = Some(Panel::Delete(id));
            }
            Msg::ForceStop(id) => {
                if matches!(self.panel, Some(Panel::ForceStop(current)) if current == id)
                    && !self.busy
                {
                    return self.change(Mutation::ForceStop { vm_id: id });
                }
            }
            Msg::Stop(id) => {
                if matches!(self.panel, Some(Panel::Stop(current)) if current == id) && !self.busy {
                    return self.change(Mutation::State {
                        vm_id: id,
                        desired: Lifecycle::Stopped,
                    });
                }
            }
            Msg::EditGpu(selection) => self.edit_gpu = selection,
            Msg::ToggleEditGpu(pci, enabled) => {
                if !self.busy
                    && let Some(Panel::EditWorkspace(id)) = self.panel
                    && let Some(vm) = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.vms.iter().find(|v| v.id == id))
                {
                    let mut selected = self.edit_gpu.target_gpus(vm);
                    selected.retain(|current| !current.eq_ignore_ascii_case(&pci));
                    if enabled {
                        selected.push(pci);
                    }
                    let selected = canonical_gpu_set(selected);
                    self.edit_gpu = if selected == GpuEdit::Keep.target_gpus(vm) {
                        GpuEdit::Keep
                    } else {
                        GpuEdit::SetMany { pcis: selected }
                    };
                }
            }
            Msg::BackToEdit(id) => {
                if matches!(&self.panel,Some(Panel::ConfirmEdit { vm_id, .. }) if *vm_id==id)
                    && !self.busy
                {
                    self.error = None;
                    self.panel = Some(Panel::EditWorkspace(id));
                }
            }
            Msg::SaveWorkspace(id) => {
                if !matches!(self.panel, Some(Panel::EditWorkspace(current)) if current==id)
                    || self.busy
                    || self.snapshot_unavailable
                {
                    return Task::none();
                }
                match self.workspace_edit() {
                    Ok(changes) => {
                        let Some(vm) = self
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.vms.iter().find(|v| v.id == id))
                        else {
                            return Task::none();
                        };
                        if changes.requires_restart(vm) {
                            self.panel = Some(Panel::ConfirmEdit { vm_id: id, changes });
                        } else {
                            return self.change(Mutation::Edit {
                                vm_id: id,
                                changes,
                                restart_confirmed: false,
                            });
                        }
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            Msg::ConfirmWorkspaceEdit(id) => {
                if !self.busy
                    && !self.snapshot_unavailable
                    && let Some(Panel::ConfirmEdit { vm_id, changes }) = &self.panel
                    && *vm_id == id
                {
                    return self.change(Mutation::Edit {
                        vm_id: id,
                        changes: changes.clone(),
                        restart_confirmed: true,
                    });
                }
            }
            Msg::Select(id) => {
                if let Some(u) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.users.iter().find(|u| u.id == id))
                {
                    self.selected = Some(id);
                    self.panel = Some(Panel::Grant);
                    self.fields[Field::User as usize] = u.name.clone();
                    self.fields[Field::RamGrant as usize] =
                        (u.grant.memory_bytes / GIB).to_string();
                    self.fields[Field::CoreGrant as usize] = u.grant.cpu_cores.to_string();
                    self.fields[Field::DiskGrant as usize] = (u.grant.disk_bytes / GIB).to_string();
                    self.gpu = u.gpu_allowed;
                    self.resource_bounds = resources::FormBounds::capture(self);
                }
            }
            Msg::Grant => match (self.selected, self.grant()) {
                (Some(user_id), Ok(grant)) => {
                    return self.rpc(Request::Grant {
                        user_id,
                        grant,
                        gpu_allowed: self.gpu,
                    });
                }
                (_, Err(e)) => self.error = Some(e.to_string()),
                _ => self.error = Some("Select a user first".into()),
            },
            Msg::Invite => match self.grant() {
                Ok(grant) => {
                    return self.rpc(Request::Invite {
                        name: self.get(Field::User).into(),
                        role: Role::User,
                        grant,
                        gpu_allowed: self.gpu,
                        expires_hours: 24,
                    });
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            Msg::ToggleT3 => {
                if !(matches!(self.panel, Some(Panel::Create)) && self.create_pending.is_some()) {
                    self.install_t3 = !self.install_t3;
                    self.install_managed_host = false;
                    self.resource_bounds = resources::FormBounds::capture(self);
                }
            }
            Msg::SelectWorkload(kind) => match kind {
                Some(ApplicationKind::HomecloudHost) if !self.install_managed_host => {
                    return self.update(Msg::ToggleManagedHost);
                }
                Some(ApplicationKind::T3) if !self.install_t3 => return self.update(Msg::ToggleT3),
                None if !self.busy && self.create_pending.is_none() => {
                    self.install_t3 = false;
                    self.install_managed_host = false;
                    self.resource_bounds = resources::FormBounds::capture(self);
                }
                _ => {}
            },
            Msg::ToggleManagedHost => {
                if !self.busy
                    && self.create_pending.is_none()
                    && self
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| s.me.role == Role::Admin)
                {
                    self.install_managed_host = !self.install_managed_host;
                    self.install_t3 = false;
                    if self.install_managed_host {
                        self.fields[Field::Ram as usize] = "8".into();
                        self.fields[Field::Cores as usize] = "2".into();
                        self.fields[Field::Disk as usize] = "32".into();
                    }
                    self.resource_bounds = resources::FormBounds::capture(self);
                }
            }
            install @ (Msg::InstallApplication(_) | Msg::InstallManagedHost(_)) => {
                let (vm_id, application_kind) = match install {
                    Msg::InstallManagedHost(id) => (id, ApplicationKind::HomecloudHost),
                    Msg::InstallApplication(id) => (id, ApplicationKind::T3),
                    _ => unreachable!(),
                };
                if self.app_request.is_some() {
                    return Task::none();
                }
                self.app_request = Some(ApplicationRequest {
                    id: vm_id,
                    target: None,
                    submitting_code: false,
                });
                return self.rpc(Request::EnsureApplication {
                    vm_id,
                    application_kind,
                    idempotency_key: Uuid::new_v4(),
                });
            }
            Msg::SetupApplication(application_id, setup_action) => {
                if self.app_request.is_some() {
                    return Task::none();
                }
                self.app_request = Some(ApplicationRequest {
                    id: application_id,
                    target: Some(setup_action),
                    submitting_code: false,
                });
                self.authorization_input.clear();
                self.browser_opened = None;
                self.app_details = false;
                let request = self.rpc(Request::StartApplicationSetup {
                    application_id,
                    setup_action,
                    idempotency_key: Uuid::new_v4(),
                });
                return if setup_action != ApplicationSetupAction::ConnectT3 {
                    Task::batch([request, iced::widget::operation::snap_to_end("page-body")])
                } else {
                    request
                };
            }
            Msg::OpenAuthorization(id) => {
                let action = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.applications.iter().find(|a| a.id == id))
                    .and_then(|a| a.observation.action.as_ref())
                    .filter(|a| a.expires_at > unix_now());
                if let Some(action) = action {
                    let url = action.url.clone();
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || open_authorization(&url))
                                .await
                                .map_err(|_| "Could not open browser".into())
                                .and_then(|r| r)
                        },
                        move |r| Msg::BrowserOpened(id, r),
                    );
                }
                self.error = Some("This link expired. Request a new authorization link.".into());
            }
            Msg::BrowserOpened(id, r) => match r {
                Ok(()) => self.browser_opened = Some(id),
                Err(error) => self.error = Some(error),
            },
            Msg::ToggleGpu => self.gpu = !self.gpu,
            Msg::Revoke(peer) => return self.rpc(Request::RevokeDevice { peer }),
            Msg::CopyInvite => {
                if let Some(s) = &self.invite {
                    return iced::clipboard::write(s.clone());
                }
            }
            Msg::Ssh(id, install) => {
                if let Some(vm) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.vms.iter().find(|v| v.id == id))
                    .cloned()
                {
                    let profile = self.profile.clone();
                    self.busy = true;
                    return Task::perform(
                        async move {
                            match tokio::task::spawn_blocking(move || {
                                export_vm(profile, vm, install)
                            })
                            .await
                            {
                                Ok(r) => result(r),
                                Err(e) => Err(e.to_string()),
                            }
                        },
                        Msg::Exported,
                    );
                }
            }
            Msg::Exported(r) => {
                self.busy = false;
                match r {
                    Ok((notice, alias)) => {
                        self.notice = notice;
                        self.close_panel();
                        if let Some(alias) = alias {
                            return iced::clipboard::write(alias);
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        Task::none()
    }
    fn subscription(&self) -> Subscription<Msg> {
        iced::time::every(Duration::from_secs(5)).map(|_| Msg::Refresh)
    }
    fn view(&self) -> Element<'_, Msg> {
        ui::view(self)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn validate_authorization_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(char::is_control) {
        return Err("Invalid authorization URL".into());
    }
    Ok(())
}
fn open_authorization(url: &str) -> Result<(), String> {
    validate_authorization_url(url)?;
    let status = std::process::Command::new("open")
        .arg(url)
        .status()
        .map_err(|_| "Could not open browser".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open browser".into())
    }
}

fn export_vm(profile: PathBuf, vm: Vm, install: bool) -> anyhow::Result<(String, Option<String>)> {
    let cli = std::env::var_os("HOMECLOUD_CLI")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("homecloud"));
    anyhow::ensure!(
        cli.is_file(),
        "Install the homecloud CLI beside the app, or set HOMECLOUD_CLI"
    );
    let (config, alias) = homecloud_client::export_ssh(&profile, &vm, &cli, install)?;
    if install {
        return Ok((
            "SSH alias copied. In T3 Code, add an SSH environment and paste it.".into(),
            Some(alias),
        ));
    }
    let command = format!(
        "ssh -F {} {}",
        homecloud_client::shell_quote(&config.to_string_lossy()),
        alias
    );
    let script = format!(
        "tell application \"Terminal\" to do script \"{}\"",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    anyhow::ensure!(
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status()?
            .success(),
        "Terminal could not be opened"
    );
    Ok((format!("Opened Terminal for {}", vm.spec.name), None))
}
fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Homecloud")
        .theme(App::theme)
        .default_font(iced_m3::fonts::REGULAR)
        .subscription(App::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1180.0, 820.0),
            min_size: Some(iced::Size::new(760.0, 560.0)),
            ..Default::default()
        })
        .run()
}
