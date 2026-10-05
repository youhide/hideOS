//! `org.freedesktop.login1`, the part hideOS's software calls (see
//! ARCHITECTURE.md, "hidelogin"): the manager, each session — with
//! `session/auto`, the caller's own — seat0 and each user.
//!
//! What may be done is polkit's to say, with the actions hidelogin's policy
//! file defines under logind's names; root may do anything. A session's own
//! person may lock it and set its screen's brightness while it is shown.

use std::collections::HashMap;
use std::os::fd::OwnedFd as StdOwnedFd;
use std::sync::OnceLock;

use anyhow::Result;
use hidelogin::session::{Session, session_of};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Value};
use zbus::{Connection, fdo, interface};

use crate::daemon::{Inhibitor, Shared, lock};
use crate::power;

pub const NAME: &str = "org.freedesktop.login1";
pub const PATH: &str = "/org/freedesktop/login1";

static CONNECTION: OnceLock<Connection> = OnceLock::new();

pub fn connection() -> Option<&'static Connection> {
    CONNECTION.get()
}

/// A session's object path, as logind names it: the id escaped as
/// sd_bus_path_encode escapes it, so `1` is `_31`.
pub fn session_path(id: &str) -> OwnedObjectPath {
    let mut escaped = String::new();
    for (i, byte) in id.bytes().enumerate() {
        if byte.is_ascii_alphabetic() || (byte.is_ascii_digit() && i > 0) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("_{byte:02x}"));
        }
    }
    path(&format!("{PATH}/session/{escaped}"))
}

pub fn user_path(uid: u32) -> OwnedObjectPath {
    path(&format!("{PATH}/user/_{uid}"))
}

fn seat_path() -> OwnedObjectPath {
    path(&format!("{PATH}/seat/seat0"))
}

fn path(text: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(text.to_owned()).unwrap_or_else(|_| {
        OwnedObjectPath::from(zbus::zvariant::ObjectPath::from_static_str_unchecked("/"))
    })
}

fn no_session(what: &str) -> fdo::Error {
    fdo::Error::Failed(format!("org.freedesktop.login1.NoSuchSession: {what}"))
}

/// The caller's process and user, from the bus.
async fn caller(connection: &Connection, header: &Header<'_>) -> fdo::Result<(u32, u32)> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("a call with no sender".into()))?
        .to_owned();
    let bus = fdo::DBusProxy::new(connection).await?;
    let pid = bus
        .get_connection_unix_process_id(sender.clone().into())
        .await?;
    let uid = bus.get_connection_unix_user(sender.into()).await?;
    Ok((pid, uid))
}

/// The session a process belongs to.
fn session_of_pid(pid: u32) -> Option<String> {
    session_of(&std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
}

/// polkit's answer for `action`; root is always allowed.
async fn authorize(
    connection: &Connection,
    header: &Header<'_>,
    action: &str,
    interactive: bool,
) -> fdo::Result<()> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("a call with no sender".into()))?
        .to_owned();
    let bus = fdo::DBusProxy::new(connection).await?;
    if bus.get_connection_unix_user(sender.clone().into()).await? == 0 {
        return Ok(());
    }
    let subject = (
        "system-bus-name",
        HashMap::from([("name", Value::from(sender.as_str()))]),
    );
    let reply = connection
        .call_method(
            Some("org.freedesktop.PolicyKit1"),
            "/org/freedesktop/PolicyKit1/Authority",
            Some("org.freedesktop.PolicyKit1.Authority"),
            "CheckAuthorization",
            &(
                subject,
                action,
                HashMap::<&str, &str>::new(),
                u32::from(interactive),
                "",
            ),
        )
        .await;
    let authorized = match reply {
        Ok(message) => {
            let (authorized, _, _): (bool, bool, HashMap<String, String>) =
                message.body().deserialize()?;
            authorized
        }
        Err(_) => false,
    };
    if authorized {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(format!(
            "not authorized for {action}"
        )))
    }
}

pub struct Manager {
    shared: Shared,
}

#[interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    async fn get_session(&self, id: &str) -> fdo::Result<OwnedObjectPath> {
        match lock(&self.shared).sessions.get(id) {
            Some(s) => Ok(session_path(&s.id)),
            None => Err(no_session(id)),
        }
    }

    async fn get_session_by_pid(
        &self,
        pid: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        let pid = if pid == 0 {
            caller(connection, &header).await?.0
        } else {
            pid
        };
        let id = session_of_pid(pid).ok_or_else(|| no_session(&format!("pid {pid}")))?;
        self.get_session(&id).await
    }

    async fn get_seat(&self, id: &str) -> fdo::Result<OwnedObjectPath> {
        match id {
            "seat0" | "auto" | "" => Ok(seat_path()),
            other => Err(fdo::Error::Failed(format!(
                "org.freedesktop.login1.NoSuchSeat: {other}"
            ))),
        }
    }

    async fn get_user(&self, uid: u32) -> fdo::Result<OwnedObjectPath> {
        if lock(&self.shared).sessions.of_user(uid).is_empty() {
            return Err(fdo::Error::Failed(format!(
                "org.freedesktop.login1.NoSuchUser: {uid}"
            )));
        }
        Ok(user_path(uid))
    }

    /// Every session: id, uid, user, seat, path.
    async fn list_sessions(&self) -> Vec<(String, u32, String, String, OwnedObjectPath)> {
        lock(&self.shared)
            .sessions
            .all()
            .map(|s| {
                (
                    s.id.clone(),
                    s.uid,
                    s.user.clone(),
                    s.seat.clone().unwrap_or_default(),
                    session_path(&s.id),
                )
            })
            .collect()
    }

    async fn power_off(
        &self,
        interactive: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        authorize(
            connection,
            &header,
            "org.freedesktop.login1.power-off",
            interactive,
        )
        .await?;
        power::shutdown(self.shared.clone(), false).await;
        Ok(())
    }

    async fn reboot(
        &self,
        interactive: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        authorize(
            connection,
            &header,
            "org.freedesktop.login1.reboot",
            interactive,
        )
        .await?;
        power::shutdown(self.shared.clone(), true).await;
        Ok(())
    }

    async fn suspend(
        &self,
        interactive: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        authorize(
            connection,
            &header,
            "org.freedesktop.login1.suspend",
            interactive,
        )
        .await?;
        power::sleep(self.shared.clone(), power::Sleep::Suspend)
            .await
            .map_err(|e| fdo::Error::Failed(format!("{e:#}")))
    }

    async fn hibernate(
        &self,
        interactive: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        authorize(
            connection,
            &header,
            "org.freedesktop.login1.hibernate",
            interactive,
        )
        .await?;
        power::sleep(self.shared.clone(), power::Sleep::Hibernate)
            .await
            .map_err(|e| fdo::Error::Failed(format!("{e:#}")))
    }

    /// cosmic-osd's restart asks this with `false` first: nothing to do
    /// then. Into the firmware's setup is not offered.
    async fn set_reboot_to_firmware_setup(&self, enable: bool) -> fdo::Result<()> {
        if enable {
            Err(fdo::Error::NotSupported(
                "rebooting into the firmware's setup".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// Holds `what` — sleep, shutdown, the lid, the power key — back for as
    /// long as the returned descriptor is open: blocked, or with `delay`,
    /// delayed until the holder lets go or the delay runs out.
    async fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<OwnedFd> {
        const KNOWN: &[&str] = &[
            "shutdown",
            "sleep",
            "idle",
            "handle-power-key",
            "handle-suspend-key",
            "handle-hibernate-key",
            "handle-lid-switch",
        ];
        let whats: Vec<String> = what.split(':').map(str::to_owned).collect();
        if whats.is_empty() || whats.iter().any(|w| !KNOWN.contains(&w.as_str())) {
            return Err(fdo::Error::InvalidArgs(format!(
                "`{what}` is not what can be inhibited"
            )));
        }
        let delay_ok = whats.iter().all(|w| w == "sleep" || w == "shutdown");
        match mode {
            "block" => {}
            "delay" if delay_ok => {}
            _ => {
                return Err(fdo::Error::InvalidArgs(format!(
                    "`{mode}` is not a mode for {what}"
                )));
            }
        }
        for w in &whats {
            let action = if w.starts_with("handle-") {
                format!("org.freedesktop.login1.inhibit-{w}")
            } else {
                format!("org.freedesktop.login1.inhibit-{mode}-{w}")
            };
            authorize(connection, &header, &action, false).await?;
        }
        let (pid, uid) = caller(connection, &header).await?;
        let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
            .map_err(|e| fdo::Error::Failed(format!("a pipe: {e}")))?;
        let id = lock(&self.shared).add_inhibitor(Inhibitor {
            what: whats,
            who: who.to_owned(),
            why: why.to_owned(),
            mode: mode.to_owned(),
            uid,
            pid,
        });
        let shared = self.shared.clone();
        tokio::spawn(released(shared, id, read));
        Ok(OwnedFd::from(write))
    }

    /// What is held back, by whom and why: what, who, why, mode, uid, pid.
    async fn list_inhibitors(&self) -> Vec<(String, String, String, String, u32, u32)> {
        lock(&self.shared)
            .inhibitors
            .values()
            .map(|i| {
                (
                    i.what.join(":"),
                    i.who.clone(),
                    i.why.clone(),
                    i.mode.clone(),
                    i.uid,
                    i.pid,
                )
            })
            .collect()
    }

    #[zbus(property)]
    async fn lid_closed(&self) -> bool {
        lock(&self.shared).lid_closed
    }

    #[zbus(property)]
    async fn preparing_for_sleep(&self) -> bool {
        lock(&self.shared).sleeping
    }

    #[zbus(signal)]
    pub async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn prepare_for_shutdown(emitter: &SignalEmitter<'_>, start: bool)
    -> zbus::Result<()>;

    #[zbus(signal)]
    async fn session_new(
        emitter: &SignalEmitter<'_>,
        id: &str,
        path: OwnedObjectPath,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn session_removed(
        emitter: &SignalEmitter<'_>,
        id: &str,
        path: OwnedObjectPath,
    ) -> zbus::Result<()>;
}

/// The inhibitor goes when every copy of its descriptor is closed: the
/// read end of its pipe then reads end of file.
async fn released(shared: Shared, id: u64, read: StdOwnedFd) {
    let file = tokio::fs::File::from_std(std::fs::File::from(read));
    let mut file = file;
    let mut buffer = [0u8; 16];
    loop {
        match tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    lock(&shared).inhibitors.remove(&id);
    power::inhibitors_changed();
}

/// A session, or with no id the caller's own: `session/auto`.
pub struct SessionObject {
    shared: Shared,
    id: Option<String>,
}

impl SessionObject {
    async fn session(
        &self,
        connection: &Connection,
        header: Option<&Header<'_>>,
    ) -> fdo::Result<Session> {
        let id = match &self.id {
            Some(id) => id.clone(),
            None => {
                let header = header.ok_or_else(|| no_session("auto, outside a call"))?;
                let (pid, _) = caller(connection, header).await?;
                session_of_pid(pid)
                    .ok_or_else(|| no_session(&format!("auto: pid {pid} has none")))?
            }
        };
        lock(&self.shared)
            .sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| no_session(&id))
    }

    fn is_active(&self, id: &str) -> bool {
        lock(&self.shared).sessions.is_active(id)
    }

    /// Only the session's own person, or root.
    async fn owned(&self, connection: &Connection, header: &Header<'_>) -> fdo::Result<Session> {
        let session = self.session(connection, Some(header)).await?;
        let (_, uid) = caller(connection, header).await?;
        if uid != 0 && uid != session.uid {
            return Err(fdo::Error::AccessDenied(format!(
                "session {} is not uid {uid}'s",
                session.id
            )));
        }
        Ok(session)
    }
}

#[interface(name = "org.freedesktop.login1.Session")]
impl SessionObject {
    /// Asks the session to lock: the Lock signal, on the session's own
    /// path, which its screen locker listens to.
    #[zbus(name = "Lock")]
    async fn lock_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        let session = self.owned(connection, &header).await?;
        emit_lock(connection, &session.id, true).await;
        Ok(())
    }

    #[zbus(name = "Unlock")]
    async fn unlock_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        let session = self.owned(connection, &header).await?;
        emit_lock(connection, &session.id, false).await;
        Ok(())
    }

    /// A display's or a keyboard light's brightness, for the session on
    /// screen: as elogind, `backlight` and `leds` devices only.
    async fn set_brightness(
        &self,
        subsystem: &str,
        name: &str,
        brightness: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        let session = self.owned(connection, &header).await?;
        if !self.is_active(&session.id) {
            return Err(fdo::Error::AccessDenied(
                "the session is not the one on screen".into(),
            ));
        }
        if !matches!(subsystem, "backlight" | "leds")
            || name.is_empty()
            || name.contains('/')
            || name.starts_with('.')
        {
            return Err(fdo::Error::InvalidArgs(format!("{subsystem}/{name}")));
        }
        let device = std::path::Path::new("/sys/class")
            .join(subsystem)
            .join(name);
        let max: u32 = std::fs::read_to_string(device.join("max_brightness"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("no {subsystem} device {name}")))?;
        std::fs::write(device.join("brightness"), brightness.min(max).to_string())
            .map_err(|e| fdo::Error::Failed(format!("{}: {e}", device.display())))
    }

    /// libseat's logind backend sets the type; hideOS's uses seatd's
    /// protocol, so the type stays what PAM said.
    async fn set_type(
        &self,
        _kind: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        self.owned(connection, &header).await.map(|_| ())
    }

    #[zbus(signal, name = "Lock")]
    async fn lock_signal(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "Unlock")]
    async fn unlock_signal(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    async fn id(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self.session(connection, header.as_ref()).await?.id)
    }

    #[zbus(property)]
    async fn user(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<(u32, OwnedObjectPath)> {
        let s = self.session(connection, header.as_ref()).await?;
        Ok((s.uid, user_path(s.uid)))
    }

    #[zbus(property)]
    async fn name(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self.session(connection, header.as_ref()).await?.user)
    }

    #[zbus(property)]
    async fn seat(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<(String, OwnedObjectPath)> {
        let s = self.session(connection, header.as_ref()).await?;
        Ok(match s.seat {
            Some(seat) => (seat, seat_path()),
            None => (String::new(), path("/")),
        })
    }

    #[zbus(property, name = "VTNr")]
    async fn vt_nr(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<u32> {
        Ok(self
            .session(connection, header.as_ref())
            .await?
            .vt
            .unwrap_or(0))
    }

    #[zbus(property, name = "TTY")]
    async fn tty(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self.session(connection, header.as_ref()).await?.tty)
    }

    #[zbus(property)]
    async fn class(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self
            .session(connection, header.as_ref())
            .await?
            .class
            .as_str()
            .to_owned())
    }

    #[zbus(property, name = "Type")]
    async fn kind(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self
            .session(connection, header.as_ref())
            .await?
            .kind
            .as_str()
            .to_owned())
    }

    #[zbus(property)]
    async fn desktop(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self.session(connection, header.as_ref()).await?.desktop)
    }

    #[zbus(property)]
    async fn service(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        Ok(self.session(connection, header.as_ref()).await?.service)
    }

    #[zbus(property)]
    async fn remote(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<bool> {
        Ok(self.session(connection, header.as_ref()).await?.remote)
    }

    #[zbus(property)]
    async fn leader(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<u32> {
        Ok(self.session(connection, header.as_ref()).await?.leader)
    }

    #[zbus(property)]
    async fn active(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<bool> {
        let s = self.session(connection, header.as_ref()).await?;
        Ok(self.is_active(&s.id))
    }

    #[zbus(property)]
    async fn state(
        &self,
        #[zbus(header)] header: Option<Header<'_>>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<String> {
        let s = self.session(connection, header.as_ref()).await?;
        Ok(if self.is_active(&s.id) {
            "active"
        } else {
            "online"
        }
        .to_owned())
    }

    #[zbus(property)]
    async fn idle_hint(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn locked_hint(&self) -> bool {
        false
    }
}

async fn emit_lock(connection: &Connection, id: &str, lock: bool) {
    let path = session_path(id);
    let Ok(emitter) = SignalEmitter::new(connection, path) else {
        return;
    };
    let sent = if lock {
        SessionObject::lock_signal(&emitter).await
    } else {
        SessionObject::unlock_signal(&emitter).await
    };
    if let Err(error) = sent {
        eprintln!("hidelogin: signalling session {id}: {error}");
    }
}

pub struct SeatObject {
    shared: Shared,
}

#[interface(name = "org.freedesktop.login1.Seat")]
impl SeatObject {
    /// To another VT, for the session on screen or root.
    async fn switch_to(
        &self,
        vt: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        let (_, uid) = caller(connection, &header).await?;
        let allowed = uid == 0
            || lock(&self.shared)
                .sessions
                .active()
                .is_some_and(|s| s.uid == uid);
        if !allowed {
            return Err(fdo::Error::AccessDenied(
                "only the session on screen switches".into(),
            ));
        }
        let vt = i32::try_from(vt).map_err(|_| fdo::Error::InvalidArgs(format!("VT {vt}")))?;
        let tty = crate::daemon::open_tty(vt).map_err(|e| fdo::Error::Failed(format!("{e:#}")))?;
        crate::sys::vt_activate(&tty, vt).map_err(|e| fdo::Error::Failed(format!("VT {vt}: {e}")))
    }

    #[zbus(property)]
    async fn id(&self) -> String {
        "seat0".to_owned()
    }

    #[zbus(property)]
    async fn active_session(&self) -> (String, OwnedObjectPath) {
        match lock(&self.shared).sessions.active() {
            Some(s) => (s.id.clone(), session_path(&s.id)),
            None => (String::new(), path("/")),
        }
    }

    #[zbus(property)]
    async fn sessions(&self) -> Vec<(String, OwnedObjectPath)> {
        lock(&self.shared)
            .sessions
            .all()
            .filter(|s| s.seat.as_deref() == Some("seat0"))
            .map(|s| (s.id.clone(), session_path(&s.id)))
            .collect()
    }

    #[zbus(property, name = "CanGraphical")]
    async fn can_graphical(&self) -> bool {
        true
    }

    #[zbus(property, name = "CanTTY")]
    async fn can_tty(&self) -> bool {
        true
    }
}

pub struct UserObject {
    shared: Shared,
    uid: u32,
}

#[interface(name = "org.freedesktop.login1.User")]
impl UserObject {
    #[zbus(property, name = "UID")]
    async fn uid(&self) -> u32 {
        self.uid
    }

    #[zbus(property)]
    async fn name(&self) -> String {
        lock(&self.shared)
            .sessions
            .of_user(self.uid)
            .first()
            .map(|s| s.user.clone())
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn state(&self) -> String {
        let daemon = lock(&self.shared);
        let sessions = daemon.sessions.of_user(self.uid);
        if sessions.iter().any(|s| daemon.sessions.is_active(&s.id)) {
            "active"
        } else if sessions.is_empty() {
            "offline"
        } else {
            "online"
        }
        .to_owned()
    }

    #[zbus(property)]
    async fn sessions(&self) -> Vec<(String, OwnedObjectPath)> {
        lock(&self.shared)
            .sessions
            .of_user(self.uid)
            .iter()
            .map(|s| (s.id.clone(), session_path(&s.id)))
            .collect()
    }

    #[zbus(property)]
    async fn runtime_path(&self) -> String {
        format!("/run/user/{}", self.uid)
    }
}

/// On the bus, as the manager, seat0 and `session/auto`.
pub async fn serve(shared: Shared) -> Result<()> {
    let connection = zbus::connection::Builder::system()?
        .name(NAME)?
        .serve_at(
            PATH,
            Manager {
                shared: shared.clone(),
            },
        )?
        .serve_at(
            format!("{PATH}/seat/seat0"),
            SeatObject {
                shared: shared.clone(),
            },
        )?
        .serve_at(
            format!("{PATH}/session/auto"),
            SessionObject {
                shared: shared.clone(),
                id: None,
            },
        )?
        .build()
        .await?;
    let _ = CONNECTION.set(connection);
    Ok(())
}

/// A new session's object, and its user's.
pub async fn session_added(shared: &Shared, id: &str) {
    let Some(connection) = connection() else {
        return;
    };
    let server = connection.object_server();
    let uid = lock(shared).sessions.get(id).map(|s| s.uid);
    let _ = server
        .at(
            session_path(id),
            SessionObject {
                shared: shared.clone(),
                id: Some(id.to_owned()),
            },
        )
        .await;
    if let Some(uid) = uid {
        let _ = server
            .at(
                user_path(uid),
                UserObject {
                    shared: shared.clone(),
                    uid,
                },
            )
            .await;
    }
    if let Ok(emitter) = SignalEmitter::new(connection, PATH) {
        let _ = Manager::session_new(&emitter, id, session_path(id)).await;
    }
    active_changed(shared).await;
}

/// A session gone: its object, and its user's when it was the last.
pub async fn session_removed(shared: &Shared, id: &str) {
    let Some(connection) = connection() else {
        return;
    };
    let server = connection.object_server();
    let _ = server.remove::<SessionObject, _>(session_path(id)).await;
    let users: Vec<u32> = {
        let daemon = lock(shared);
        daemon.sessions.all().map(|s| s.uid).collect()
    };
    // Users with no session left lose their object.
    for path_uid in known_users(shared) {
        if !users.contains(&path_uid) {
            let _ = server.remove::<UserObject, _>(user_path(path_uid)).await;
        }
    }
    if let Ok(emitter) = SignalEmitter::new(connection, PATH) {
        let _ = Manager::session_removed(&emitter, id, session_path(id)).await;
    }
    active_changed(shared).await;
}

/// The uids that have had a user object: every uid from /etc/passwd's
/// regular users is cheap enough to try.
fn known_users(_shared: &Shared) -> Vec<u32> {
    std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split(':').nth(2)?.parse().ok())
        .collect()
}

/// Which session is active may have changed: every session's `Active`
/// and seat0's `ActiveSession`, announced.
pub async fn active_changed(shared: &Shared) {
    let Some(connection) = connection() else {
        return;
    };
    let server = connection.object_server();
    let ids: Vec<String> = lock(shared).sessions.all().map(|s| s.id.clone()).collect();
    for id in ids {
        if let Ok(iface) = server
            .interface::<_, SessionObject>(session_path(&id))
            .await
        {
            let object = iface.get().await;
            let _ = object.active_changed(iface.signal_emitter()).await;
            let _ = object.state_changed(iface.signal_emitter()).await;
        }
    }
    if let Ok(iface) = server.interface::<_, SeatObject>(seat_path()).await {
        let object = iface.get().await;
        let _ = object.active_session_changed(iface.signal_emitter()).await;
    }
}

/// Every session asked to lock, as the configuration's `lock` action does.
pub async fn lock_all(shared: &Shared) {
    let Some(connection) = connection() else {
        return;
    };
    let ids: Vec<String> = lock(shared).sessions.all().map(|s| s.id.clone()).collect();
    for id in ids {
        emit_lock(connection, &id, true).await;
    }
}

/// PrepareForSleep or PrepareForShutdown, to everyone listening.
pub async fn announce(sleep: bool, start: bool) {
    let Some(connection) = connection() else {
        return;
    };
    let Ok(emitter) = SignalEmitter::new(connection, PATH) else {
        return;
    };
    let sent = if sleep {
        Manager::prepare_for_sleep(&emitter, start).await
    } else {
        Manager::prepare_for_shutdown(&emitter, start).await
    };
    if let Err(error) = sent {
        eprintln!(
            "hidelogin: announcing {}: {error}",
            if sleep { "sleep" } else { "shutdown" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_paths_are_escaped_as_logind_escapes_them() {
        assert_eq!(
            session_path("1").as_str(),
            "/org/freedesktop/login1/session/_31"
        );
        assert_eq!(
            session_path("12").as_str(),
            "/org/freedesktop/login1/session/_312"
        );
        assert_eq!(
            session_path("c1").as_str(),
            "/org/freedesktop/login1/session/c1"
        );
        assert_eq!(
            user_path(1000).as_str(),
            "/org/freedesktop/login1/user/_1000"
        );
    }
}
