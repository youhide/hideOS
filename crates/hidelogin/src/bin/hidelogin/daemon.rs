//! The daemon's state, behind one lock, and the side effects of what the
//! library decides: devices opened and revoked, VTs handed over, sessions'
//! cgroups, runtime directories and state files.
//!
//! The lock is never held across an await: everything here is quick and
//! synchronous, and the tasks in the other modules take it, act, and let
//! go.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write as _;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use hidelogin::conf::Config;
use hidelogin::seat::{ClientId, DeviceKind, Effect, Open, Seat};
use hidelogin::seatd::{self, Reply};
use hidelogin::session::{self, Request, Session, Sessions};
use rustix::fs::{Mode, OFlags};

use crate::sys;

/// The root of the cgroup hierarchy.
const CGROUPS: &str = "/sys/fs/cgroup";

pub type Shared = Arc<Mutex<Daemon>>;

/// A compositor connected over seatd's socket.
pub struct SeatClient {
    pub stream: UnixStream,
    pub pid: u32,
    pub uid: u32,
    pub session: String,
    /// The descriptors of its devices, by id.
    pub devices: HashMap<i32, OwnedFd>,
}

/// Something holding sleep, shutdown, the lid or the power key back.
pub struct Inhibitor {
    pub what: Vec<String>,
    pub who: String,
    pub why: String,
    pub mode: String,
    pub uid: u32,
    pub pid: u32,
}

pub struct Daemon {
    pub config: Config,
    pub sessions: Sessions,
    pub seat: Seat,
    pub clients: HashMap<ClientId, SeatClient>,
    next_client: ClientId,
    pub inhibitors: BTreeMap<u64, Inhibitor>,
    next_inhibitor: u64,
    pub lid_closed: bool,
    /// Whether a sleep is under way, between PrepareForSleep's two signals.
    pub sleeping: bool,
}

pub fn lock(shared: &Shared) -> MutexGuard<'_, Daemon> {
    // A panic while holding the lock leaves the state as it was at the
    // panic; going on with it beats taking every session down.
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Daemon {
    pub fn new(config: Config) -> Daemon {
        Daemon {
            config,
            sessions: Sessions::new(),
            seat: Seat::new(),
            clients: HashMap::new(),
            next_client: 1,
            inhibitors: BTreeMap::new(),
            next_inhibitor: 1,
            lid_closed: false,
            sleeping: false,
        }
    }

    pub fn add_client(&mut self, client: SeatClient) -> ClientId {
        let id = self.next_client;
        self.next_client += 1;
        self.clients.insert(id, client);
        id
    }

    pub fn add_inhibitor(&mut self, inhibitor: Inhibitor) -> u64 {
        let id = self.next_inhibitor;
        self.next_inhibitor += 1;
        self.inhibitors.insert(id, inhibitor);
        id
    }

    /// Whether anything blocks `what`; with `delay`, holds it back for now.
    pub fn inhibited(&self, what: &str, mode: &str) -> bool {
        self.inhibitors
            .values()
            .any(|i| i.mode == mode && i.what.iter().any(|w| w == what))
    }

    /// A seatd request from `client`, carried out.
    pub fn seatd_request(&mut self, client: ClientId, request: seatd::Request) {
        let effects = match request {
            seatd::Request::OpenSeat => match current_vt() {
                Ok(vt) => {
                    if self.client_may_take(client, vt) {
                        self.seat.open_seat(client, vt)
                    } else {
                        vec![Effect::Send(
                            client,
                            Reply::Error(rustix::io::Errno::PERM.raw_os_error()),
                        )]
                    }
                }
                Err(error) => {
                    eprintln!("hidelogin: the current VT: {error}");
                    vec![Effect::Send(client, Reply::Error(error.raw_os_error()))]
                }
            },
            seatd::Request::CloseSeat => self.seat.close_seat(client, true),
            seatd::Request::OpenDevice(path) => self.open_device(client, &path),
            seatd::Request::CloseDevice(id) => self.seat.close_device(client, id),
            seatd::Request::DisableSeat => self.seat.disable_acked(client),
            seatd::Request::SwitchSession(vt) => self.seat.switch_session(client, vt),
            seatd::Request::Ping => vec![Effect::Send(client, Reply::Pong)],
        };
        self.apply(effects);
    }

    /// The client's connection ended: its seat and devices go with it.
    pub fn client_gone(&mut self, client: ClientId) {
        let effects = self.seat.close_seat(client, false);
        self.apply(effects);
        self.clients.remove(&client);
    }

    /// Only a process of the login session on the VT being shown, and that
    /// session's own user or root, may become the seat's client. seatd lets
    /// anyone who can open its socket; hidelogin asks who it is.
    fn client_may_take(&self, client: ClientId, vt: i32) -> bool {
        let Some(c) = self.clients.get(&client) else {
            return false;
        };
        let Some(session) = self.sessions.get(&c.session) else {
            return false;
        };
        let vt_matches = session.vt.and_then(|v| i32::try_from(v).ok()) == Some(vt);
        let owner = c.uid == 0 || c.uid == session.uid;
        let on_seat = session.seat.as_deref() == Some("seat0");
        if !(vt_matches && owner && on_seat) {
            eprintln!(
                "hidelogin: pid {} (uid {}) refused the seat: session {} is uid {} on VT {:?}, VT {vt} is shown",
                c.pid, c.uid, session.id, session.uid, session.vt
            );
        }
        vt_matches && owner && on_seat
    }

    fn open_device(&mut self, client: ClientId, path: &str) -> Vec<Effect> {
        let errno =
            |e: rustix::io::Errno| vec![Effect::Send(client, Reply::Error(e.raw_os_error()))];
        // Canonical, so a link cannot name another device.
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) => {
                return errno(
                    rustix::io::Errno::from_io_error(&error).unwrap_or(rustix::io::Errno::NOENT),
                );
            }
        };
        let Some(canonical) = canonical.to_str().map(str::to_owned) else {
            return errno(rustix::io::Errno::NOENT);
        };
        match self.seat.open_device(client, &canonical) {
            Err(e) => vec![Effect::Send(client, Reply::Error(e))],
            Ok(Open::Again(id)) => self.send_device(client, id),
            Ok(Open::New { id, kind }) => {
                let opened = rustix::fs::open(
                    canonical.as_str(),
                    OFlags::RDWR
                        | OFlags::NOCTTY
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC
                        | OFlags::NONBLOCK,
                    Mode::empty(),
                );
                match opened {
                    Ok(fd) => {
                        if kind == DeviceKind::Drm
                            && let Err(error) = sys::drm_set_master(&fd)
                        {
                            // A render node has no master; the client may
                            // still use it.
                            eprintln!("hidelogin: DRM master of {canonical}: {error}");
                        }
                        self.seat.opened(client, id, &canonical, kind);
                        if let Some(c) = self.clients.get_mut(&client) {
                            c.devices.insert(id, fd);
                        }
                        self.send_device(client, id)
                    }
                    Err(e) => errno(e),
                }
            }
        }
    }

    fn send_device(&mut self, client: ClientId, id: i32) -> Vec<Effect> {
        let Some(c) = self.clients.get(&client) else {
            return Vec::new();
        };
        let Some(fd) = c.devices.get(&id) else {
            return vec![Effect::Send(
                client,
                Reply::Error(rustix::io::Errno::BADF.raw_os_error()),
            )];
        };
        if let Err(error) = send(
            &c.stream,
            &seatd::encode(&Reply::DeviceOpened(id)),
            Some(fd.as_fd()),
        ) {
            eprintln!("hidelogin: sending device {id} to pid {}: {error}", c.pid);
        }
        Vec::new()
    }

    /// Carries out what the seat decided, in order.
    pub fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            if let Err(error) = self.apply_one(&effect) {
                eprintln!("hidelogin: {effect:?}: {error:#}");
            }
        }
    }

    fn apply_one(&mut self, effect: &Effect) -> Result<()> {
        match effect {
            Effect::Send(client, reply) => {
                if let Some(c) = self.clients.get(client) {
                    send(&c.stream, &seatd::encode(reply), None)?;
                }
            }
            Effect::Activate(client, id) => {
                if let Some(fd) = self.clients.get(client).and_then(|c| c.devices.get(id)) {
                    sys::drm_set_master(fd)?;
                }
            }
            Effect::Deactivate(client, id) => {
                if let Some((path, fd)) = self.device(*client, *id) {
                    revoke(&path, fd)?;
                }
            }
            Effect::Close(client, id) => {
                let path = self.device(*client, *id).map(|(path, _)| path);
                if let Some(fd) = self
                    .clients
                    .get_mut(client)
                    .and_then(|c| c.devices.remove(id))
                {
                    if let Some(path) = path {
                        revoke(&path, &fd)?;
                    }
                    drop(fd);
                }
            }
            Effect::OpenVt(vt) => {
                let tty = open_tty(*vt)?;
                sys::vt_set_process_switching(&tty, true)?;
                sys::vt_set_keyboard(&tty, false)?;
                sys::vt_set_graphics(&tty, true)?;
            }
            Effect::CloseVt(vt) => {
                let tty = open_tty(*vt)?;
                sys::vt_set_process_switching(&tty, false)?;
                sys::vt_set_keyboard(&tty, true)?;
                sys::vt_set_graphics(&tty, false)?;
            }
            Effect::SwitchVt { from, to } => {
                let tty = open_tty(*from)?;
                sys::vt_set_process_switching(&tty, true)?;
                sys::vt_activate(&tty, *to)?;
            }
            Effect::AckVt { release } => {
                let vt = current_vt()?;
                sys::vt_ack(open_tty(vt)?, *release)?;
            }
        }
        Ok(())
    }

    /// The path the seat knows device `id` by, and its descriptor.
    fn device(&self, client: ClientId, id: i32) -> Option<(String, &OwnedFd)> {
        let c = self.clients.get(&client)?;
        let fd = c.devices.get(&id)?;
        let path = fs::read_link(format!(
            "/proc/self/fd/{}",
            rustix::fd::AsRawFd::as_raw_fd(fd)
        ))
        .ok()?
        .to_string_lossy()
        .into_owned();
        Some((path, fd))
    }

    /// A session opens: its cgroup with the leader in it, delegated to its
    /// user; the user's runtime directory; the state files.
    pub fn open_session(&mut self, request: Request, gid: u32) -> Result<Session> {
        let session = self.sessions.open(request);
        let made = (|| {
            let cgroup = Path::new(CGROUPS).join(session.cgroup());
            fs::DirBuilder::new()
                .recursive(true)
                .create(&cgroup)
                .with_context(|| format!("creating {}", cgroup.display()))?;
            fs::write(cgroup.join("cgroup.procs"), session.leader.to_string())
                .with_context(|| format!("moving {} into {}", session.leader, cgroup.display()))?;
            // Delegated as cgroup v2 delegates: the directory and the files
            // that move processes and enable controllers, the user's.
            for path in [
                cgroup.clone(),
                cgroup.join("cgroup.procs"),
                cgroup.join("cgroup.threads"),
                cgroup.join("cgroup.subtree_control"),
            ] {
                let _ = rustix::fs::chown(
                    &path,
                    Some(rustix::fs::Uid::from_raw(session.uid)),
                    Some(rustix::fs::Gid::from_raw(gid)),
                );
            }
            runtime_dir(session.uid, gid)?;
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(error) = made {
            self.sessions.close(&session.id);
            return Err(error);
        }
        self.write_state();
        Ok(session)
    }

    /// A session ends: its state, and with the user's last session, the
    /// runtime directory. Its cgroup goes once it is empty; processes the
    /// person left running keep it until then.
    pub fn close_session(&mut self, id: &str) -> Option<Session> {
        let session = self.sessions.close(id)?;
        let _ = fs::remove_dir(Path::new(CGROUPS).join(session.cgroup()));
        if self.sessions.of_user(session.uid).is_empty() {
            let runtime = PathBuf::from(format!("/run/user/{}", session.uid));
            // The document portal's FUSE mount, and any other, detached
            // first: removing through one is refused.
            let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
            for point in hidelogin::mounts::mounts_under(&mountinfo, &runtime.to_string_lossy()) {
                if let Err(error) =
                    rustix::mount::unmount(&point, rustix::mount::UnmountFlags::DETACH)
                {
                    eprintln!("hidelogin: detaching {point}: {error}");
                }
            }
            if let Err(error) = fs::remove_dir_all(&runtime) {
                eprintln!("hidelogin: removing {}: {error}", runtime.display());
            }
        }
        let _ = fs::remove_file(Path::new(session::STATE).join("sessions").join(id));
        self.write_state();
        Some(session)
    }

    /// The current VT changed: who is active, and the files that say so.
    pub fn set_current_vt(&mut self, vt: Option<u32>) {
        self.sessions.set_current_vt(vt);
        self.write_state();
    }

    /// Every state file, written anew and renamed into place, so a reader
    /// never sees half of one and inotify sees the change.
    pub fn write_state(&self) {
        let root = Path::new(session::STATE);
        let write = |dir: &str, name: &str, text: &str| {
            let dir = root.join(dir);
            let _ = fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(&dir);
            let partial = dir.join(format!(".{name}"));
            let done = fs::File::create(&partial)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .and_then(|()| fs::set_permissions(&partial, fs::Permissions::from_mode(0o644)))
                .and_then(|()| fs::rename(&partial, dir.join(name)));
            if let Err(error) = done {
                eprintln!("hidelogin: writing {}/{name}: {error}", dir.display());
            }
        };
        let mut users = std::collections::BTreeSet::new();
        for s in self.sessions.all() {
            write("sessions", &s.id, &s.render(self.sessions.is_active(&s.id)));
            users.insert(s.uid);
        }
        // Users whose last session closed lose their file.
        if let Ok(entries) = fs::read_dir(root.join("users")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.parse::<u32>().is_ok_and(|uid| !users.contains(&uid)) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        for uid in users {
            if let Some(text) = self.sessions.render_user(uid) {
                write("users", &uid.to_string(), &text);
            }
        }
        write("seats", "seat0", &self.sessions.render_seat());
    }
}

/// Revokes access to a device by what it is.
fn revoke(path: &str, fd: &OwnedFd) -> Result<()> {
    match hidelogin::seat::device_kind(path) {
        Some(DeviceKind::Drm) => sys::drm_drop_master(fd)?,
        Some(DeviceKind::Evdev) => sys::evdev_revoke(fd)?,
        Some(DeviceKind::Hidraw) => sys::hidraw_revoke(fd)?,
        None => bail!("{path} is no seat device"),
    }
    Ok(())
}

/// `/run/user/UID`, the user's, 0700, made if missing.
fn runtime_dir(uid: u32, gid: u32) -> Result<()> {
    let path = PathBuf::from(format!("/run/user/{uid}"));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create("/run/user")?;
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("creating {}", path.display())),
    }
    rustix::fs::chown(
        &path,
        Some(rustix::fs::Uid::from_raw(uid)),
        Some(rustix::fs::Gid::from_raw(gid)),
    )?;
    Ok(())
}

/// A VT's terminal, `/dev/ttyN`.
pub fn open_tty(vt: i32) -> Result<OwnedFd> {
    let path = format!("/dev/tty{vt}");
    rustix::fs::open(
        path.as_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("opening {path}"))
}

/// The VT the kernel shows.
pub fn current_vt() -> rustix::io::Result<i32> {
    let tty0 = rustix::fs::open(
        "/dev/tty0",
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    sys::current_vt(tty0)
}

/// One message on a seatd connection, with a descriptor when there is one.
pub fn send(
    stream: &UnixStream,
    message: &[u8],
    fd: Option<std::os::fd::BorrowedFd<'_>>,
) -> Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let iov = [std::io::IoSlice::new(message)];
    let fds;
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if let Some(fd) = fd {
        fds = [fd];
        if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
            bail!("no room for a descriptor in the message");
        }
    }
    let sent = sendmsg(stream, &iov, &mut control, SendFlags::NOSIGNAL)?;
    if sent != message.len() {
        bail!("sent {sent} of {} bytes", message.len());
    }
    Ok(())
}
