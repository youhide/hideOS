//! `pam_hidelogin.so`, in a PAM stack's session phase: the session greetd
//! opens — the greeter's, then the person's — registered with hidelogin.
//!
//! At `pam_sm_open_session`, in the process PAM runs in (greetd's worker,
//! as root), the module sends hidelogin what PAM knows of the session and
//! puts what comes back in PAM's environment: `XDG_SESSION_ID` and
//! `XDG_RUNTIME_DIR`, which the session's programs inherit. The connection
//! then stays open, held in PAM's data, until PAM is done with the session;
//! hidelogin sees it close and ends the session.
//!
//! A failure is said and the session goes on without: the stack lists the
//! module as `optional`, and a login with no session record beats none.
//!
//! The module does not link libpam: the `pam_*` functions it calls are
//! found in the process that loads it, which is libpam itself. The builder
//! has no libpam to link against, and every process that can load a PAM
//! module already has it.

#![allow(unsafe_code)]
#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use hidelogin::session::{Class, Kind, Request, parse_fields, render_request};

const PAM_SUCCESS: c_int = 0;
const PAM_SESSION_ERR: c_int = 14;
const PAM_SERVICE: c_int = 1;
const PAM_USER: c_int = 2;
const PAM_TTY: c_int = 3;
const PAM_RHOST: c_int = 4;

/// The name the connection is kept under in PAM's data.
const DATA: &CStr = c"hidelogin-session";
const SOCKET: &str = "/run/hidelogin/pam.sock";

#[allow(non_camel_case_types)]
type pam_handle_t = c_void;

unsafe extern "C" {
    fn pam_get_item(pamh: *const pam_handle_t, item_type: c_int, item: *mut *const c_void)
    -> c_int;
    fn pam_getenv(pamh: *mut pam_handle_t, name: *const c_char) -> *const c_char;
    fn pam_putenv(pamh: *mut pam_handle_t, name_value: *const c_char) -> c_int;
    fn pam_set_data(
        pamh: *mut pam_handle_t,
        module_data_name: *const c_char,
        data: *mut c_void,
        cleanup: Option<unsafe extern "C" fn(*mut pam_handle_t, *mut c_void, c_int)>,
    ) -> c_int;
    fn pam_syslog(pamh: *const pam_handle_t, priority: c_int, fmt: *const c_char, ...);
}

/// A message in the system log, as PAM modules say things.
fn say(pamh: *mut pam_handle_t, text: &str) {
    let Ok(text) = CString::new(text) else {
        return;
    };
    // SAFETY: pamh is PAM's handle; the format is a literal "%s" and its one
    // argument a NUL-terminated string.
    unsafe {
        pam_syslog(
            pamh,
            libc::LOG_ERR,
            c"pam_hidelogin: %s".as_ptr(),
            text.as_ptr(),
        )
    };
}

/// A PAM item as a string, empty when unset.
fn item(pamh: *mut pam_handle_t, kind: c_int) -> String {
    let mut value: *const c_void = std::ptr::null();
    // SAFETY: pamh is PAM's handle and value a writable pointer.
    if unsafe { pam_get_item(pamh, kind, &mut value) } != PAM_SUCCESS || value.is_null() {
        return String::new();
    }
    // SAFETY: string items are NUL-terminated strings PAM owns.
    unsafe { CStr::from_ptr(value.cast()) }
        .to_string_lossy()
        .into_owned()
}

/// A variable from PAM's environment, then the process's, empty when unset:
/// greetd puts XDG_SEAT, XDG_VTNR and the session's class there.
fn env(pamh: *mut pam_handle_t, name: &str) -> String {
    if let Ok(c) = CString::new(name) {
        // SAFETY: pamh is PAM's handle; c is NUL-terminated.
        let value = unsafe { pam_getenv(pamh, c.as_ptr()) };
        if !value.is_null() {
            // SAFETY: PAM returns a NUL-terminated string it owns.
            return unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned();
        }
    }
    std::env::var(name).unwrap_or_default()
}

fn putenv(pamh: *mut pam_handle_t, name: &str, value: &str) -> Result<(), String> {
    let pair =
        CString::new(format!("{name}={value}")).map_err(|_| format!("{name} holds a NUL"))?;
    // SAFETY: pamh is PAM's handle; PAM copies the string.
    if unsafe { pam_putenv(pamh, pair.as_ptr()) } == PAM_SUCCESS {
        Ok(())
    } else {
        Err(format!("setting {name}"))
    }
}

/// The user's uid, from the password database.
fn uid_of(user: &str) -> Result<u32, String> {
    let name = CString::new(user).map_err(|_| "a user name with a NUL".to_owned())?;
    let mut entry: libc::passwd = unsafe_zeroed_passwd();
    let mut buffer = vec![0 as c_char; 16384];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to memory of the size passed beside it.
    let r = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if r != 0 || found.is_null() {
        return Err(format!("no user {user}"));
    }
    Ok(entry.pw_uid)
}

fn unsafe_zeroed_passwd() -> libc::passwd {
    // SAFETY: passwd is plain C data; all zeros is a valid value of it,
    // pointers null, until getpwnam_r fills it.
    unsafe { std::mem::zeroed() }
}

unsafe extern "C" fn close_connection(_pamh: *mut pam_handle_t, data: *mut c_void, _status: c_int) {
    if !data.is_null() {
        // SAFETY: the data is the Box this module put there, taken back
        // once: PAM calls the cleanup once per datum.
        drop(unsafe { Box::from_raw(data.cast::<UnixStream>()) });
    }
}

fn open(pamh: *mut pam_handle_t) -> Result<(), String> {
    let user = item(pamh, PAM_USER);
    if user.is_empty() {
        return Err("no user".into());
    }
    let request = Request {
        uid: uid_of(&user)?,
        user,
        // SAFETY: getpid has no preconditions.
        leader: u32::try_from(unsafe { libc::getpid() })
            .map_err(|_| "a negative pid".to_owned())?,
        service: item(pamh, PAM_SERVICE),
        class: Class::parse(&env(pamh, "XDG_SESSION_CLASS")).map_err(|e| e.to_string())?,
        kind: Kind::parse(&env(pamh, "XDG_SESSION_TYPE")).unwrap_or(Kind::Unspecified),
        desktop: env(pamh, "XDG_SESSION_DESKTOP"),
        seat: Some(env(pamh, "XDG_SEAT")).filter(|s| !s.is_empty()),
        vt: env(pamh, "XDG_VTNR").parse().ok(),
        tty: item(pamh, PAM_TTY),
        remote: !item(pamh, PAM_RHOST).is_empty(),
    };
    let text = render_request(&request);
    // A line break in a value would end the line early: refused.
    if text.trim_end_matches('\n').contains("\n\n") || text.matches('\n').count() != 12 {
        return Err("a value holds a line break".into());
    }
    let mut stream =
        UnixStream::connect(SOCKET).map_err(|e| format!("connecting to {SOCKET}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    stream
        .write_all(text.as_bytes())
        .map_err(|e| format!("sending: {e}"))?;
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    while !reply.ends_with(b"\n\n") {
        match stream.read(&mut byte) {
            Ok(1) => reply.push(byte[0]),
            Ok(_) => return Err("hidelogin closed the connection".into()),
            Err(e) => return Err(format!("reading the reply: {e}")),
        }
        if reply.len() > 4096 {
            return Err("a reply too long".into());
        }
    }
    let _ = stream.set_read_timeout(None);
    let reply = String::from_utf8_lossy(&reply).into_owned();
    let fields = parse_fields(&reply);
    if let Some(error) = fields.get("ERROR") {
        return Err(format!("hidelogin: {error}"));
    }
    let id = fields.get("ID").ok_or("no session id in the reply")?;
    putenv(pamh, "XDG_SESSION_ID", id)?;
    if let Some(runtime) = fields.get("RUNTIME") {
        putenv(pamh, "XDG_RUNTIME_DIR", runtime)?;
    }
    for (key, name) in [("SEAT", "XDG_SEAT"), ("VTNR", "XDG_VTNR")] {
        if let Some(value) = fields.get(key).filter(|v| !v.is_empty()) {
            putenv(pamh, name, value)?;
        }
    }
    putenv(pamh, "XDG_SESSION_TYPE", request.kind.as_str())?;
    putenv(pamh, "XDG_SESSION_CLASS", request.class.as_str())?;
    // The connection, kept for as long as PAM keeps the session.
    let data = Box::into_raw(Box::new(stream)).cast::<c_void>();
    // SAFETY: pamh is PAM's handle; the cleanup takes the Box back.
    if unsafe { pam_set_data(pamh, DATA.as_ptr(), data, Some(close_connection)) } != PAM_SUCCESS {
        // SAFETY: PAM did not take it: ours to free.
        unsafe { close_connection(pamh, data, 0) };
        return Err("keeping the connection".into());
    }
    Ok(())
}

/// # Safety
/// Called by PAM with its handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_open_session(
    pamh: *mut pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    match std::panic::catch_unwind(|| open(pamh)) {
        Ok(Ok(())) => PAM_SUCCESS,
        Ok(Err(error)) => {
            say(pamh, &error);
            PAM_SESSION_ERR
        }
        Err(_) => PAM_SESSION_ERR,
    }
}

/// The session ends: the connection closes, and with it the session.
///
/// # Safety
/// Called by PAM with its handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_close_session(
    pamh: *mut pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    // Replacing the datum runs its cleanup, which closes the connection.
    // SAFETY: pamh is PAM's handle; no new data, no cleanup.
    unsafe { pam_set_data(pamh, DATA.as_ptr(), std::ptr::null_mut(), None) };
    PAM_SUCCESS
}
