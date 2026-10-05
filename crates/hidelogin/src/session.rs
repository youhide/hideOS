//! Login sessions: what the PAM module registers, which one is active, and
//! how others learn about them.
//!
//! A session is a cgroup, `hidelogin.slice/session-ID`, beside oxinit's
//! own slice: its leader is moved there when the session opens, every
//! process it starts inherits it, and so `/proc/PID/cgroup` says which
//! session any process belongs to — the question polkit asks before every
//! decision. The daemon writes each session, user and seat to a file under
//! `/run/hidelogin`, replaced by a rename, and the sd-login library answers
//! from those files without asking the daemon.

use std::collections::BTreeMap;

use thiserror::Error;

/// The directory of state files.
pub const STATE: &str = "/run/hidelogin";
/// The cgroup that holds the sessions, under the root.
pub const SLICE: &str = "hidelogin.slice";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("`{0}` is not a session class")]
    Class(String),
    #[error("`{0}` is not a session type")]
    Kind(String),
    #[error("no `{0}`")]
    Missing(&'static str),
    #[error("`{key}` is `{value}`, not a number")]
    Number { key: &'static str, value: String },
}

/// What a session is for, as logind's `Class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    User,
    Greeter,
    LockScreen,
    Background,
}

impl Class {
    pub fn parse(text: &str) -> Result<Class, SessionError> {
        Ok(match text {
            // logind's default, and what greetd leaves unset for a login.
            "" | "user" => Class::User,
            "greeter" => Class::Greeter,
            "lock-screen" => Class::LockScreen,
            "background" => Class::Background,
            other => return Err(SessionError::Class(other.to_owned())),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Class::User => "user",
            Class::Greeter => "greeter",
            Class::LockScreen => "lock-screen",
            Class::Background => "background",
        }
    }
}

/// What it shows on, as logind's `Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wayland,
    X11,
    Tty,
    Unspecified,
}

impl Kind {
    pub fn parse(text: &str) -> Result<Kind, SessionError> {
        Ok(match text {
            "wayland" => Kind::Wayland,
            "x11" => Kind::X11,
            "tty" => Kind::Tty,
            "" | "unspecified" => Kind::Unspecified,
            other => return Err(SessionError::Kind(other.to_owned())),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Wayland => "wayland",
            Kind::X11 => "x11",
            Kind::Tty => "tty",
            Kind::Unspecified => "unspecified",
        }
    }
}

/// What the PAM module tells the daemon when a session opens: its own
/// process, which becomes the leader, and what greetd put in PAM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub uid: u32,
    pub user: String,
    pub leader: u32,
    pub service: String,
    pub class: Class,
    pub kind: Kind,
    pub desktop: String,
    pub seat: Option<String>,
    pub vt: Option<u32>,
    pub tty: String,
    pub remote: bool,
}

/// A session, as the daemon keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub uid: u32,
    pub user: String,
    pub leader: u32,
    pub service: String,
    pub class: Class,
    pub kind: Kind,
    pub desktop: String,
    pub seat: Option<String>,
    pub vt: Option<u32>,
    pub tty: String,
    pub remote: bool,
    /// Opened after every other on its VT so far: the newer of two
    /// sessions on one VT — the person's, as the greeter's ends — is shown.
    pub serial: u64,
}

impl Session {
    /// Its cgroup, from the root of the hierarchy.
    pub fn cgroup(&self) -> String {
        format!("{SLICE}/session-{}", self.id)
    }

    /// Its line in the session file, as sd-login reads it back.
    pub fn render(&self, active: bool) -> String {
        let mut text = String::new();
        let mut line = |key: &str, value: &str| {
            text.push_str(key);
            text.push('=');
            text.push_str(value);
            text.push('\n');
        };
        line("UID", &self.uid.to_string());
        line("USER", &self.user);
        line("LEADER", &self.leader.to_string());
        line("SERVICE", &self.service);
        line("CLASS", self.class.as_str());
        line("TYPE", self.kind.as_str());
        line("DESKTOP", &self.desktop);
        line("SEAT", self.seat.as_deref().unwrap_or(""));
        line(
            "VTNR",
            &self.vt.map(|vt| vt.to_string()).unwrap_or_default(),
        );
        line("TTY", &self.tty);
        line("REMOTE", if self.remote { "1" } else { "0" });
        line("ACTIVE", if active { "1" } else { "0" });
        line("STATE", if active { "active" } else { "online" });
        text
    }
}

/// The session `cgroup_file` — a process's `/proc/PID/cgroup` — places it
/// in, if any: the cgroup v2 line, `0::/hidelogin.slice/session-ID/…`.
pub fn session_of(cgroup_file: &str) -> Option<String> {
    let path = cgroup_file
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let rest = path
        .strip_prefix('/')?
        .strip_prefix(SLICE)?
        .strip_prefix("/session-")?;
    let id = rest.split('/').next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_owned())
}

/// Every session, and which is active.
#[derive(Debug, Default)]
pub struct Sessions {
    sessions: BTreeMap<String, Session>,
    next_id: u64,
    serial: u64,
    /// The VT the kernel shows on seat0, when known.
    current_vt: Option<u32>,
}

impl Sessions {
    pub fn new() -> Sessions {
        Sessions {
            next_id: 1,
            ..Sessions::default()
        }
    }

    pub fn open(&mut self, request: Request) -> Session {
        let id = self.next_id.to_string();
        self.next_id += 1;
        self.serial += 1;
        let session = Session {
            id: id.clone(),
            uid: request.uid,
            user: request.user,
            leader: request.leader,
            service: request.service,
            class: request.class,
            kind: request.kind,
            desktop: request.desktop,
            seat: request.seat,
            vt: request.vt,
            tty: request.tty,
            remote: request.remote,
            serial: self.serial,
        };
        self.sessions.insert(id, session.clone());
        session
    }

    pub fn close(&mut self, id: &str) -> Option<Session> {
        self.sessions.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    pub fn set_current_vt(&mut self, vt: Option<u32>) {
        self.current_vt = vt;
    }

    /// The session shown on seat0: the newest on the current VT.
    pub fn active(&self) -> Option<&Session> {
        let vt = self.current_vt?;
        self.sessions
            .values()
            .filter(|s| s.seat.as_deref() == Some("seat0") && s.vt == Some(vt) && !s.remote)
            .max_by_key(|s| s.serial)
    }

    pub fn is_active(&self, id: &str) -> bool {
        self.active().is_some_and(|s| s.id == id)
    }

    /// A user's sessions, in the order they opened.
    pub fn of_user(&self, uid: u32) -> Vec<&Session> {
        let mut sessions: Vec<&Session> = self.sessions.values().filter(|s| s.uid == uid).collect();
        sessions.sort_by_key(|s| s.serial);
        sessions
    }

    /// The user's file: `STATE`, which session is the display, and the
    /// lists sd-login hands out.
    pub fn render_user(&self, uid: u32) -> Option<String> {
        let sessions = self.of_user(uid);
        let first = sessions.first()?;
        let active: Vec<&str> = sessions
            .iter()
            .filter(|s| self.is_active(&s.id))
            .map(|s| s.id.as_str())
            .collect();
        // The graphical session, newest first; else the newest of any kind.
        let display = sessions
            .iter()
            .rev()
            .find(|s| s.kind == Kind::Wayland || s.kind == Kind::X11)
            .or(sessions.last())
            .map(|s| s.id.as_str())
            .unwrap_or("");
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        let seats = |only_active: bool| {
            let mut seats: Vec<&str> = sessions
                .iter()
                .filter(|s| !only_active || self.is_active(&s.id))
                .filter_map(|s| s.seat.as_deref())
                .collect();
            seats.dedup();
            seats.join(" ")
        };
        Some(format!(
            "NAME={}\nSTATE={}\nRUNTIME=/run/user/{uid}\nDISPLAY={display}\nSESSIONS={}\nACTIVE_SESSIONS={}\nSEATS={}\nACTIVE_SEATS={}\n",
            first.user,
            if active.is_empty() {
                "online"
            } else {
                "active"
            },
            ids.join(" "),
            active.join(" "),
            seats(false),
            seats(true),
        ))
    }

    /// seat0's file: the active session and every session on it.
    pub fn render_seat(&self) -> String {
        let on_seat: Vec<&str> = self
            .sessions
            .values()
            .filter(|s| s.seat.as_deref() == Some("seat0"))
            .map(|s| s.id.as_str())
            .collect();
        let active = self.active();
        format!(
            "IS_SEAT0=1\nCAN_TTY=1\nCAN_GRAPHICAL=1\nACTIVE={}\nACTIVE_UID={}\nSESSIONS={}\n",
            active.map(|s| s.id.as_str()).unwrap_or(""),
            active.map(|s| s.uid.to_string()).unwrap_or_default(),
            on_seat.join(" "),
        )
    }
}

/// A `KEY=value` file — a state file, or a message between the PAM module
/// and the daemon — as a map. Lines without `=` are ignored.
pub fn parse_fields(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .collect()
}

/// The PAM module's request as it is sent: `KEY=value` lines.
pub fn render_request(request: &Request) -> String {
    format!(
        "UID={}\nUSER={}\nLEADER={}\nSERVICE={}\nCLASS={}\nTYPE={}\nDESKTOP={}\nSEAT={}\nVTNR={}\nTTY={}\nREMOTE={}\n\n",
        request.uid,
        request.user,
        request.leader,
        request.service,
        request.class.as_str(),
        request.kind.as_str(),
        request.desktop,
        request.seat.as_deref().unwrap_or(""),
        request.vt.map(|vt| vt.to_string()).unwrap_or_default(),
        request.tty,
        if request.remote { "1" } else { "0" },
    )
}

/// The daemon's reading of a request. Values with a newline cannot be in
/// one: the module refuses them before sending.
pub fn parse_request(text: &str) -> Result<Request, SessionError> {
    let fields = parse_fields(text);
    let get = |key: &'static str| fields.get(key).copied().ok_or(SessionError::Missing(key));
    let number = |key: &'static str| -> Result<u32, SessionError> {
        let value = get(key)?;
        value.parse().map_err(|_| SessionError::Number {
            key,
            value: value.to_owned(),
        })
    };
    let optional = |key: &'static str| fields.get(key).copied().filter(|v| !v.is_empty());
    Ok(Request {
        uid: number("UID")?,
        user: get("USER")?.to_owned(),
        leader: number("LEADER")?,
        service: get("SERVICE")?.to_owned(),
        class: Class::parse(get("CLASS")?)?,
        kind: Kind::parse(get("TYPE")?)?,
        desktop: optional("DESKTOP").unwrap_or("").to_owned(),
        seat: optional("SEAT").map(str::to_owned),
        vt: match optional("VTNR") {
            Some(_) => Some(number("VTNR")?),
            None => None,
        },
        tty: optional("TTY").unwrap_or("").to_owned(),
        remote: optional("REMOTE") == Some("1"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(uid: u32, class: Class, vt: u32) -> Request {
        Request {
            uid,
            user: if uid == 0 {
                "root".into()
            } else {
                format!("u{uid}")
            },
            leader: 100 + uid,
            service: "cosmic-greeter".into(),
            class,
            kind: Kind::Wayland,
            desktop: "COSMIC".into(),
            seat: Some("seat0".into()),
            vt: Some(vt),
            tty: "tty1".into(),
            remote: false,
        }
    }

    #[test]
    fn a_process_belongs_to_the_session_its_cgroup_names() {
        assert_eq!(
            session_of("0::/hidelogin.slice/session-3\n"),
            Some("3".into())
        );
        assert_eq!(
            session_of("0::/hidelogin.slice/session-12/app.service\n"),
            Some("12".into())
        );
        assert_eq!(session_of("0::/oxinit.slice/greetd.service\n"), None);
        assert_eq!(session_of("0::/hidelogin.slice/session-\n"), None);
        assert_eq!(session_of("0::/hidelogin.slice/session-../x\n"), None);
        // A v1 line is not the unified hierarchy's.
        assert_eq!(
            session_of("1:name=elogind:/hidelogin.slice/session-3\n"),
            None
        );
    }

    #[test]
    fn the_newest_session_on_the_shown_vt_is_active() {
        let mut sessions = Sessions::new();
        let greeter = sessions.open(request(0, Class::Greeter, 1));
        // Nothing is active until the VT is known.
        assert!(sessions.active().is_none());
        sessions.set_current_vt(Some(1));
        assert_eq!(
            sessions.active().map(|s| s.id.clone()),
            Some(greeter.id.clone())
        );
        // The person's session opens on the same VT as the greeter ends.
        let person = sessions.open(request(1000, Class::User, 1));
        assert!(sessions.is_active(&person.id));
        assert!(!sessions.is_active(&greeter.id));
        sessions.close(&greeter.id);
        assert!(sessions.is_active(&person.id));
        // Another VT shown: nobody's.
        sessions.set_current_vt(Some(2));
        assert!(sessions.active().is_none());
    }

    #[test]
    fn files_say_what_sd_login_asks() {
        let mut sessions = Sessions::new();
        sessions.set_current_vt(Some(1));
        let s = sessions.open(request(1000, Class::User, 1));
        let fields_text = s.render(true);
        let fields = parse_fields(&fields_text);
        assert_eq!(fields.get("UID"), Some(&"1000"));
        assert_eq!(fields.get("SEAT"), Some(&"seat0"));
        assert_eq!(fields.get("ACTIVE"), Some(&"1"));
        assert_eq!(fields.get("CLASS"), Some(&"user"));
        let user_text = sessions.render_user(1000).unwrap();
        let user = parse_fields(&user_text);
        assert_eq!(user.get("STATE"), Some(&"active"));
        assert_eq!(user.get("DISPLAY"), Some(&s.id.as_str()));
        assert_eq!(user.get("ACTIVE_SEATS"), Some(&"seat0"));
        assert!(sessions.render_user(4242).is_none());
        let seat_text = sessions.render_seat();
        let seat = parse_fields(&seat_text);
        assert_eq!(seat.get("ACTIVE"), Some(&s.id.as_str()));
        assert_eq!(seat.get("ACTIVE_UID"), Some(&"1000"));
        assert_eq!(s.cgroup(), format!("hidelogin.slice/session-{}", s.id));
    }

    #[test]
    fn a_request_goes_from_the_module_to_the_daemon_whole() {
        let sent = request(1000, Class::User, 1);
        assert_eq!(parse_request(&render_request(&sent)), Ok(sent.clone()));
        let mut bare = request(0, Class::Greeter, 1);
        bare.seat = None;
        bare.vt = None;
        bare.kind = Kind::Unspecified;
        assert_eq!(parse_request(&render_request(&bare)), Ok(bare));
        assert_eq!(parse_request("UID=1\n"), Err(SessionError::Missing("USER")));
        assert!(matches!(
            parse_request("UID=x\nUSER=a\n"),
            Err(SessionError::Number { key: "UID", .. })
        ));
        assert!(parse_request(&render_request(&sent).replace("CLASS=user", "CLASS=root")).is_err());
    }
}
