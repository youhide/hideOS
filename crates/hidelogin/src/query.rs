//! The answers sd-login's functions give, from hidelogin's state files and
//! `/proc`: what libhidelogin-sd hands polkit, NetworkManager and
//! WirePlumber. Errors are errno values, as sd-login returns them negated.

use std::fs;
use std::path::PathBuf;

use crate::session::{self, parse_fields, session_of};

pub const ENODATA: i32 = 61;
pub const ENXIO: i32 = 6;
pub const ESRCH: i32 = 3;
pub const EINVAL: i32 = 22;

/// Where to look: `/run/hidelogin` and `/proc`, or a test's directories.
#[derive(Debug, Clone)]
pub struct Query {
    pub state: PathBuf,
    pub proc: PathBuf,
}

impl Default for Query {
    fn default() -> Query {
        Query {
            state: PathBuf::from(session::STATE),
            proc: PathBuf::from("/proc"),
        }
    }
}

/// An id that cannot climb out of the state directory.
fn plain(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

impl Query {
    fn read(&self, dir: &str, name: &str) -> Option<String> {
        fs::read_to_string(self.state.join(dir).join(name)).ok()
    }

    /// The session a process belongs to; `pid` 0 is the caller.
    pub fn session_of_pid(&self, pid: u32) -> Result<String, i32> {
        let who = if pid == 0 {
            "self".to_owned()
        } else {
            pid.to_string()
        };
        let cgroup = fs::read_to_string(self.proc.join(who).join("cgroup")).map_err(|_| ESRCH)?;
        let id = session_of(&cgroup).ok_or(ENODATA)?;
        // A cgroup left from a session that ended is no session.
        if self.read("sessions", &id).is_none() {
            return Err(ENODATA);
        }
        Ok(id)
    }

    /// `session`, or the caller's own when none is named.
    fn resolve(&self, session: Option<&str>) -> Result<String, i32> {
        match session {
            Some(id) if plain(id) => Ok(id.to_owned()),
            Some(_) => Err(EINVAL),
            None => self.session_of_pid(0),
        }
    }

    fn session_field(&self, session: Option<&str>, key: &str) -> Result<String, i32> {
        let id = self.resolve(session)?;
        let text = self.read("sessions", &id).ok_or(ENXIO)?;
        let fields = parse_fields(&text);
        fields.get(key).map(|v| (*v).to_owned()).ok_or(ENODATA)
    }

    pub fn session_is_active(&self, session: Option<&str>) -> Result<bool, i32> {
        Ok(self.session_field(session, "ACTIVE")? == "1")
    }

    pub fn session_state(&self, session: Option<&str>) -> Result<String, i32> {
        self.session_field(session, "STATE")
    }

    pub fn session_uid(&self, session: Option<&str>) -> Result<u32, i32> {
        self.session_field(session, "UID")?
            .parse()
            .map_err(|_| ENODATA)
    }

    pub fn session_seat(&self, session: Option<&str>) -> Result<String, i32> {
        let seat = self.session_field(session, "SEAT")?;
        if seat.is_empty() {
            Err(ENODATA)
        } else {
            Ok(seat)
        }
    }

    /// The user of the session a process is in.
    pub fn owner_uid_of_pid(&self, pid: u32) -> Result<u32, i32> {
        let id = self.session_of_pid(pid)?;
        self.session_uid(Some(&id))
    }

    fn user_field(&self, uid: u32, key: &str) -> Option<String> {
        let text = self.read("users", &uid.to_string())?;
        parse_fields(&text).get(key).map(|v| (*v).to_owned())
    }

    /// `active`, `online`, or `offline` for a user with no session.
    pub fn uid_state(&self, uid: u32) -> String {
        self.user_field(uid, "STATE")
            .unwrap_or_else(|| "offline".to_owned())
    }

    pub fn uid_display(&self, uid: u32) -> Result<String, i32> {
        self.user_field(uid, "DISPLAY")
            .filter(|d| !d.is_empty())
            .ok_or(ENODATA)
    }

    /// The user's sessions; with `require_active`, only the active one.
    pub fn uid_sessions(&self, uid: u32, require_active: bool) -> Vec<String> {
        let key = if require_active {
            "ACTIVE_SESSIONS"
        } else {
            "SESSIONS"
        };
        words(self.user_field(uid, key))
    }

    pub fn uid_seats(&self, uid: u32, require_active: bool) -> Vec<String> {
        let key = if require_active {
            "ACTIVE_SEATS"
        } else {
            "SEATS"
        };
        words(self.user_field(uid, key))
    }

    /// The directories a monitor of `category` watches: sd-login's
    /// categories, `None` for all of them.
    pub fn monitored(&self, category: Option<&str>) -> Result<Vec<PathBuf>, i32> {
        let dirs: &[&str] = match category {
            None => &["sessions", "users", "seats"],
            Some("session") => &["sessions"],
            Some("uid") => &["users"],
            Some("seat") => &["seats"],
            // Machines are systemd-machined's; there are none to watch.
            Some("machine") => &[],
            Some(_) => return Err(EINVAL),
        };
        Ok(dirs.iter().map(|d| self.state.join(d)).collect())
    }
}

fn words(field: Option<String>) -> Vec<String> {
    field
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Class, Kind, Request, Sessions};

    struct Fixture {
        query: Query,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.query.state.parent().unwrap());
        }
    }

    fn fixture(name: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("hidelogin-query-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let query = Query {
            state: root.join("run"),
            proc: root.join("proc"),
        };
        let mut sessions = Sessions::new();
        sessions.set_current_vt(Some(1));
        let s = sessions.open(Request {
            uid: 1000,
            user: "ada".into(),
            leader: 4242,
            service: "cosmic-greeter".into(),
            class: Class::User,
            kind: Kind::Wayland,
            desktop: "COSMIC".into(),
            seat: Some("seat0".into()),
            vt: Some(1),
            tty: "tty1".into(),
            remote: false,
        });
        for dir in ["sessions", "users", "seats"] {
            fs::create_dir_all(query.state.join(dir)).unwrap();
        }
        fs::write(query.state.join("sessions").join(&s.id), s.render(true)).unwrap();
        fs::write(
            query.state.join("users/1000"),
            sessions.render_user(1000).unwrap(),
        )
        .unwrap();
        fs::write(query.state.join("seats/seat0"), sessions.render_seat()).unwrap();
        // A process in the session, one outside any, and the caller.
        for (pid, cgroup) in [
            (
                "4242",
                format!("0::/hidelogin.slice/session-{}/app\n", s.id),
            ),
            ("1", "0::/oxinit.slice/greetd.service\n".to_owned()),
            ("self", format!("0::/hidelogin.slice/session-{}\n", s.id)),
            ("77", "0::/hidelogin.slice/session-99\n".to_owned()),
        ] {
            fs::create_dir_all(query.proc.join(pid)).unwrap();
            fs::write(query.proc.join(pid).join("cgroup"), cgroup).unwrap();
        }
        Fixture { query }
    }

    #[test]
    fn processes_and_sessions() {
        let f = fixture("pids");
        let q = &f.query;
        assert_eq!(q.session_of_pid(4242), Ok("1".into()));
        assert_eq!(q.session_of_pid(1), Err(ENODATA));
        assert_eq!(q.session_of_pid(31337), Err(ESRCH));
        // A cgroup whose session ended.
        assert_eq!(q.session_of_pid(77), Err(ENODATA));
        assert_eq!(q.owner_uid_of_pid(4242), Ok(1000));
        // No session named: the caller's.
        assert_eq!(q.session_uid(None), Ok(1000));
        assert_eq!(q.session_is_active(Some("1")), Ok(true));
        assert_eq!(q.session_state(Some("1")), Ok("active".into()));
        assert_eq!(q.session_seat(Some("1")), Ok("seat0".into()));
        assert_eq!(q.session_is_active(Some("2")), Err(ENXIO));
        assert_eq!(q.session_uid(Some("../users/1000")), Err(EINVAL));
    }

    #[test]
    fn users() {
        let f = fixture("users");
        let q = &f.query;
        assert_eq!(q.uid_state(1000), "active");
        assert_eq!(q.uid_state(1001), "offline");
        assert_eq!(q.uid_display(1000), Ok("1".into()));
        assert_eq!(q.uid_display(1001), Err(ENODATA));
        assert_eq!(q.uid_sessions(1000, false), vec!["1".to_owned()]);
        assert_eq!(q.uid_sessions(1000, true), vec!["1".to_owned()]);
        assert_eq!(q.uid_seats(1000, true), vec!["seat0".to_owned()]);
        assert!(q.uid_sessions(1001, false).is_empty());
        assert_eq!(
            q.monitored(Some("uid")).unwrap(),
            vec![q.state.join("users")]
        );
        assert_eq!(q.monitored(None).unwrap().len(), 3);
        assert_eq!(q.monitored(Some("nonsense")), Err(EINVAL));
    }
}
