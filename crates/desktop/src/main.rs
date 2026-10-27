use homecloud_client::{Client, default_profile};
use homecloud_core::*;
use iced::{Subscription, Task};
use iced_m3::{Element, Theme, text_field};
mod ui;
#[cfg(test)]
mod visual_tests;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Workspaces,
    Host,
    People,
    Activity,
}
#[derive(Clone)]
enum Panel {
    Create,
    Vm(Uuid),
    Resize(Uuid),
    Invite,
    Grant,
    Invitation,
    Delete(Uuid),
    Revoke(String),
    T3(Uuid),
}
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
    Navigate(Page),
    Open(Panel),
    Close,
    DismissNotice,
    ToggleHardware,
    ToggleIdentity,
    ToggleMotion,
    Retry(Uuid),
    Pair,
    Loaded(Result<Arc<Client>, String>),
    Refresh,
    Snapshot(Result<Box<Snapshot>, String>),
    Reply(Result<Response, String>),
    Create,
    Change(Mutation),
    Delete(Uuid),
    Resize(Uuid),
    Select(Uuid),
    Grant,
    Invite,
    ToggleGpu,
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
    page: Page,
    panel: Option<Panel>,
    last_panel: Option<Panel>,
    hardware_details: bool,
    identity_details: bool,
    reduced_motion: bool,
}
fn result<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}
impl App {
    fn blank(profile: PathBuf) -> Self {
        let mut fields: [String; 12] = Default::default();
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
            client: None,
            snapshot: None,
            fields,
            error: None,
            notice: String::new(),
            busy: false,
            refreshing: false,
            selected: None,
            gpu: false,
            invite: None,
            page: Page::Workspaces,
            panel: None,
            last_panel: None,
            hardware_details: false,
            identity_details: false,
            reduced_motion: false,
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
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        app.fields[Field::Key as usize] = std::fs::read_to_string(home.join(".ssh/id_ed25519.pub"))
            .unwrap_or_default()
            .trim()
            .into();
        (app, task)
    }
    fn theme(&self) -> Theme {
        Theme::from_accent(iced::Color::from_rgb8(101, 151, 235), true)
            .expressive()
            .reduced_motion(self.reduced_motion)
    }
    fn close_panel(&mut self) {
        self.last_panel = self.panel.take();
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
            Msg::Navigate(page) => {
                self.page = page;
                self.close_panel();
                self.error = None;
            }
            Msg::Open(panel) => {
                self.error = None;
                if let Panel::Resize(id) = panel
                    && let Some(vm) = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.vms.iter().find(|v| v.id == id))
                {
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
            Msg::ToggleMotion => self.reduced_motion = !self.reduced_motion,
            Msg::Retry(operation_id) => return self.rpc(Request::RetryOperation { operation_id }),
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
                        self.notice.clear();
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
                    Ok(s) => {
                        self.snapshot = Some(*s);
                        if self
                            .error
                            .as_deref()
                            .is_some_and(|s| s.starts_with("Host unavailable:"))
                        {
                            self.error = None;
                        }
                    }
                    Err(e) => self.error = Some(format!("Host unavailable: {e}")),
                }
            }
            Msg::Reply(r) => {
                self.busy = false;
                match r {
                    Ok(Response::Invitation { token, .. }) => {
                        self.invite = Some(token);
                        self.panel = Some(Panel::Invitation);
                        self.notice.clear();
                    }
                    Ok(Response::Operation(_)) => {
                        self.close_panel();
                        self.notice = "Request submitted. Follow its progress in Activity.".into();
                    }
                    Ok(_) => {
                        self.close_panel();
                        self.notice = "Changes saved.".into();
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
                return self.change(m);
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
                    self.panel = Some(Panel::Grant);
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
