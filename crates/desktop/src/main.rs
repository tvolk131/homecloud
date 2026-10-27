use homecloud_client::{Client, default_profile};
use homecloud_core::*;
use iced::{
    Length, Subscription, Task,
    widget::{column, container, row, scrollable, text},
};
use iced_m3::{Element, Theme, button, text_field};
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy)]
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
}
#[derive(Clone)]
enum Msg {
    Edit(Field, String),
    Pair,
    Loaded(Result<Arc<Client>, String>),
    Refresh,
    Snapshot(Result<Box<Snapshot>, String>),
    Reply(Result<Response, String>),
    Create,
    Change(Mutation),
    Delete(Uuid),
    CancelDelete,
    Resize(Uuid),
    Select(Uuid),
    Grant,
    Invite,
    ToggleGpu,
    Revoke(String),
    CopyInvite,
    Ssh(Uuid, bool),
    Exported(Result<String, String>),
}
// Avoid formatting invitation codes or credentials in diagnostics.
impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Homecloud event")
    }
}
struct App {
    profile: PathBuf,
    client: Option<Arc<Client>>,
    snapshot: Option<Snapshot>,
    fields: [String; 12],
    error: Option<String>,
    notice: String,
    busy: bool,
    refreshing: bool,
    selected: Option<Uuid>,
    gpu: bool,
    invite: Option<String>,
    delete: Option<Uuid>,
}
fn result<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}
impl App {
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
        let mut fields: [String; 12] = Default::default();
        fields[Field::Name as usize] = "devbox".into();
        fields[Field::Ram as usize] = "2".into();
        fields[Field::Cores as usize] = "1".into();
        fields[Field::Disk as usize] = "20".into();
        fields[Field::RamGrant as usize] = "2".into();
        fields[Field::CoreGrant as usize] = "1".into();
        fields[Field::DiskGrant as usize] = "32".into();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        fields[Field::Key as usize] = std::fs::read_to_string(home.join(".ssh/id_ed25519.pub"))
            .unwrap_or_default()
            .trim()
            .into();
        (
            Self {
                profile,
                client: None,
                snapshot: None,
                fields,
                error: None,
                notice: "Pair using the verified public host ticket and a one-time invitation."
                    .into(),
                busy: false,
                refreshing: false,
                selected: None,
                gpu: false,
                invite: None,
                delete: None,
            },
            task,
        )
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
    fn change(&mut self, mutation: Mutation) -> Task<Msg> {
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
    fn grant(&self) -> anyhow::Result<Resources> {
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
            Msg::Edit(f, s) => self.fields[f as usize] = s,
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
                        self.notice = format!("Paired device {}", c.device_id());
                        self.client = Some(c);
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
                    Ok(s) => self.snapshot = Some(*s),
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Reply(r) => {
                self.busy = false;
                match r {
                    Ok(Response::Invitation { token, .. }) => {
                        self.invite = Some(token);
                        self.notice="Invitation ready. Share the code privately with the host ticket; it expires in 24 hours.".into()
                    }
                    Ok(Response::Operation(o)) => {
                        self.notice = format!("Queued operation {}. Progress appears below.", o.id)
                    }
                    Ok(_) => {
                        self.notice =
                            "Saved. Physical backing reconciles before VM operations.".into()
                    }
                    Err(e) => self.error = Some(e),
                }
                return self.update(Msg::Refresh);
            }
            Msg::Create => match self.sizes() {
                Ok((memory_gib, cores, disk_gib)) => {
                    return self.change(Mutation::Create {
                        spec: VmSpec {
                            name: self.get(Field::Name).into(),
                            memory_gib,
                            cores,
                            disk_gib,
                            ssh_public_key: self.get(Field::Key).trim().into(),
                        },
                    });
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            Msg::Change(m) => {
                self.delete = None;
                return self.change(m);
            }
            Msg::CancelDelete => self.delete = None,
            Msg::Delete(id) => {
                if self.delete == Some(id) {
                    self.delete = None;
                    return self.change(Mutation::State {
                        vm_id: id,
                        desired: Lifecycle::Deleted,
                    });
                }
                self.delete = Some(id)
            }
            Msg::Resize(vm_id) => match self.sizes() {
                Ok((memory_gib, cores, disk_gib)) => {
                    return self.change(Mutation::Resize {
                        vm_id,
                        memory_gib,
                        cores,
                        disk_gib,
                    });
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            Msg::Select(id) => {
                if let Some(u) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.users.iter().find(|u| u.id == id))
                {
                    self.selected = Some(id);
                    self.fields[Field::User as usize] = u.name.clone();
                    self.fields[Field::RamGrant as usize] =
                        (u.grant.memory_bytes / GIB).to_string();
                    self.fields[Field::CoreGrant as usize] = u.grant.cpu_cores.to_string();
                    self.fields[Field::DiskGrant as usize] = (u.grant.disk_bytes / GIB).to_string();
                    self.gpu = u.gpu_allowed;
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
                    Ok(s) => self.notice = s,
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
        let mut b = column![text("Homecloud").size(32), text(&self.notice).size(14)].spacing(16);
        if let Some(e) = &self.error {
            b = b.push(text(format!("Attention: {e}")));
        }
        if self.client.is_none() {
            b=b.push(text("Pair this Mac").size(24)).push(text("Verify the endpoint identity through your trusted channel before pasting the public server.json ticket."))
                .push(self.input("Public host ticket JSON",Field::Ticket))
                .push(text_field("One-time invitation",self.get(Field::Token)).secure(true).on_input(|s|Msg::Edit(Field::Token,s)))
                .push(button("Pair device").on_press(Msg::Pair).disabled(self.busy||self.get(Field::Token).is_empty()||self.get(Field::Ticket).is_empty()));
        } else if let Some(s) = &self.snapshot {
            b=b.push(row![text(format!("{} · {:?}",s.me.name,s.me.role)).size(24),button("Refresh").on_press(Msg::Refresh).disabled(self.refreshing)].spacing(24))
                .push(text(format!("Grant: {} GiB RAM · {} physical cores · {} GiB disk backing",s.me.grant.memory_bytes/GIB,s.me.grant.cpu_cores,s.me.grant.disk_bytes/GIB)))
                .push(text(format!("Committed: {} GiB RAM · {} cores · {} GiB disk (stopped VMs retain commitments)",s.me.committed.memory_bytes/GIB,s.me.committed.cpu_cores,s.me.committed.disk_bytes/GIB)));
            let i = &s.inventory;
            b = b
                .push(text(format!(
                    "{} · {} · {} GiB RAM",
                    i.hostname,
                    i.cpu_model,
                    i.memory_bytes / GIB
                )))
                .push(text(format!(
                    "ZFS {}: {} · {} GiB available",
                    i.pool,
                    i.pool_health,
                    i.pool_available_bytes / GIB
                )));
            if let Some(r) = &i.iommu {
                b = b.push(text(format!("GPU isolation / IOMMU: {}", r.state)));
                for l in r.evidence.iter().chain(&r.next_steps) {
                    b = b.push(text(l).size(13));
                }
            }
            for w in &i.warnings {
                b = b.push(text(format!("Host: {w}")));
            }
            for gpu in &i.gpus {
                b = b.push(
                    text(format!(
                        "{}: {}",
                        gpu.model,
                        gpu.telemetry.as_deref().unwrap_or("Telemetry unavailable")
                    ))
                    .size(13),
                );
            }
            b = b.push(text("Virtual machines").size(24));
            if s.vms.is_empty() {
                b = b.push(text(
                    "No VMs yet. Allocate a grant, then create your first dev box.",
                ));
            }
            for v in &s.vms {
                let id = v.id;
                let mut card = column![
                    text(&v.spec.name).size(22),
                    text(format!(
                        "{} · desired {:?} · {} GiB RAM / {} cores / {} GiB disk",
                        v.observed, v.desired, v.spec.memory_gib, v.spec.cores, v.spec.disk_gib
                    )),
                    text(id.to_string()).size(12)
                ]
                .spacing(10);
                if let Some(e) = &v.error {
                    card = card.push(text(e));
                }
                card = card.push(
                    row![
                        button("Start")
                            .on_press(Msg::Change(Mutation::State {
                                vm_id: id,
                                desired: Lifecycle::Running
                            }))
                            .disabled(self.busy),
                        button("Stop")
                            .on_press(Msg::Change(Mutation::State {
                                vm_id: id,
                                desired: Lifecycle::Stopped
                            }))
                            .disabled(self.busy),
                        button("Open Terminal")
                            .on_press(Msg::Ssh(id, false))
                            .disabled(
                                self.busy || v.observed != "running" || v.ssh_host_key.is_none()
                            ),
                        button("Set up T3 SSH alias")
                            .on_press(Msg::Ssh(id, true))
                            .disabled(self.busy || v.ssh_host_key.is_none())
                    ]
                    .spacing(8),
                );
                card = card.push(
                    row![
                        button("Resize using sizes below (restarts VM)")
                            .on_press(Msg::Resize(id))
                            .disabled(self.busy),
                        button(if self.delete == Some(id) {
                            "Confirm permanent delete"
                        } else {
                            "Delete VM"
                        })
                        .on_press(Msg::Delete(id))
                        .disabled(self.busy)
                    ]
                    .spacing(8),
                );
                if self.delete == Some(id) {
                    card = card.push(
                        row![
                            text("This permanently deletes this VM and its disk."),
                            button("Cancel deletion").on_press(Msg::CancelDelete)
                        ]
                        .spacing(8),
                    );
                }
                if let Some(pci) = &v.gpu {
                    card = card.push(
                        row![
                            text(format!("GPU reserved: {pci}")),
                            button("Detach GPU (restarts VM)")
                                .on_press(Msg::Change(Mutation::Gpu {
                                    vm_id: id,
                                    pci: None
                                }))
                                .disabled(self.busy)
                        ]
                        .spacing(8),
                    );
                } else if s.me.gpu_allowed || s.me.role == Role::Admin {
                    for gpu in &i.gpus {
                        card = card.push(
                            row![
                                text(format!("{} · {}", gpu.model, gpu.readiness.state)),
                                button("Attach GPU (restarts VM)")
                                    .on_press(Msg::Change(Mutation::Gpu {
                                        vm_id: id,
                                        pci: Some(gpu.pci.clone())
                                    }))
                                    .disabled(
                                        self.busy
                                            || gpu.readiness.state != "ready"
                                            || v.spec.memory_gib < 4
                                    )
                            ]
                            .spacing(8),
                        );
                    }
                }
                b = b.push(container(card).padding(16));
            }
            b=b.push(text("Create / resize a dev box").size(24)).push(text("Dedicated cores include SMT siblings. Resize restarts the guest; disks only grow. Additional physical disk overhead counts against your grant."))
                .push(self.input("VM name",Field::Name))
                .push(row![self.input("RAM GiB",Field::Ram),self.input("Physical cores",Field::Cores),self.input("Disk GiB",Field::Disk)].spacing(12))
                .push(self.input("Guest SSH public key",Field::Key)).push(button("Create VM").on_press(Msg::Create).disabled(self.busy));
            if s.me.role == Role::Admin {
                b = b.push(text("Administration").size(24));
                for u in &s.users {
                    b = b.push(
                        button(format!(
                            "{} · {} GiB / {} cores / {} GiB · GPU {}",
                            u.name,
                            u.grant.memory_bytes / GIB,
                            u.grant.cpu_cores,
                            u.grant.disk_bytes / GIB,
                            u.gpu_allowed
                        ))
                        .on_press(Msg::Select(u.id)),
                    );
                }
                b = b
                    .push(self.input("User display name / new invitation", Field::User))
                    .push(
                        row![
                            self.input("Grant RAM GiB", Field::RamGrant),
                            self.input("Grant cores", Field::CoreGrant),
                            self.input("Grant disk GiB", Field::DiskGrant)
                        ]
                        .spacing(12),
                    )
                    .push(
                        row![
                            button(if self.gpu {
                                "GPU allowed: yes"
                            } else {
                                "GPU allowed: no"
                            })
                            .on_press(Msg::ToggleGpu),
                            button("Save selected grant")
                                .on_press(Msg::Grant)
                                .disabled(self.busy || self.selected.is_none()),
                            button("Invite coworker")
                                .on_press(Msg::Invite)
                                .disabled(self.busy || self.get(Field::User).trim().is_empty())
                        ]
                        .spacing(8),
                    );
                if self.invite.is_some() {
                    b = b.push(button("Copy invitation code").on_press(Msg::CopyInvite));
                }
                b = b.push(text("Authorized devices").size(20));
                for d in &s.devices {
                    b = b.push(
                        row![
                            text(format!(
                                "{} · {}",
                                d.peer,
                                if d.active { "active" } else { "revoked" }
                            ))
                            .size(12),
                            button("Revoke")
                                .on_press(Msg::Revoke(d.peer.clone()))
                                .disabled(self.busy || !d.active)
                        ]
                        .spacing(8),
                    );
                }
            }
            b = b.push(text("Recent operations").size(24));
            for o in s.operations.iter().take(12) {
                b = b.push(
                    text(format!(
                        "{} · {} {}",
                        o.id,
                        o.phase,
                        o.error.as_deref().unwrap_or("")
                    ))
                    .size(13),
                );
            }
        } else {
            b = b.push(text("Connecting to your host…"));
        }
        iced_m3::focus::scope(
            container(scrollable(b.padding(28).max_width(1100)))
                .width(Length::Fill)
                .height(Length::Fill),
        )
    }
}
fn export_vm(profile: PathBuf, vm: Vm, install: bool) -> anyhow::Result<String> {
    let cli = std::env::var_os("HOMECLOUD_CLI")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("homecloud"));
    anyhow::ensure!(
        cli.is_file(),
        "Install the homecloud CLI beside the app, or set HOMECLOUD_CLI"
    );
    let (config, alias) = homecloud_client::export_ssh(&profile, &vm, &cli, install)?;
    if install {
        return Ok(format!(
            "T3 SSH alias installed: {alias}. Add it as an SSH environment in T3 Code."
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
    Ok(format!("Opened Terminal for {}", vm.spec.name))
}
fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Homecloud")
        .theme(|_: &App| Theme::dark())
        .default_font(iced_m3::fonts::REGULAR)
        .subscription(App::subscription)
        .window_size((1100.0, 850.0))
        .run()
}
