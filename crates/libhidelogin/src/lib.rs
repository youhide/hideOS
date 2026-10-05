//! sd-login's C functions — the ones polkit, NetworkManager and WirePlumber
//! call — answered by hidelogin's `query`. Strings and lists go back in
//! memory from `malloc`, which the caller frees, as sd-login's do; errors
//! are negative errno values, as sd-login's are. A monitor is an inotify
//! descriptor on the state directories, handed out as sd-login hands out
//! its own: the descriptor plus one, as a pointer.
//!
//! This is the one crate of hidelogin's that is C's to call, and so all
//! unsafe: every function checks its pointers before it writes through them.

#![allow(unsafe_code)]
// inotify, pidfds, abstract sockets: Linux's, where the library is used.
#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString, c_char, c_int, c_void};

use hidelogin::query::{EINVAL, Query};

/// sd-login's monitor type, opaque.
pub type Monitor = c_void;

fn query() -> Query {
    Query::default()
}

/// A C string copied into `malloc`'s memory, or null.
fn dup(text: &str) -> *mut c_char {
    let Ok(c) = CString::new(text) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `c` is a valid NUL-terminated string for the call's duration;
    // strdup returns memory the caller frees, or null.
    unsafe { libc::strdup(c.as_ptr()) }
}

/// Writes `text`, duplicated, through `out` when it is not null.
///
/// # Safety
/// `out`, if not null, points to writable memory for a pointer.
unsafe fn give(out: *mut *mut c_char, text: &str) -> c_int {
    if out.is_null() {
        return 0;
    }
    let copy = dup(text);
    if copy.is_null() {
        return -libc::ENOMEM;
    }
    // SAFETY: out is not null, and the caller promised it is writable.
    unsafe { *out = copy };
    0
}

/// A NULL-terminated array of duplicated strings through `out`, when not
/// null; the count either way.
///
/// # Safety
/// `out`, if not null, points to writable memory for a pointer.
unsafe fn give_list(out: *mut *mut *mut c_char, items: &[String]) -> c_int {
    let count = c_int::try_from(items.len()).unwrap_or(c_int::MAX);
    if out.is_null() {
        return count;
    }
    let size = std::mem::size_of::<*mut c_char>() * (items.len() + 1);
    // SAFETY: malloc with a size; the result is checked for null.
    let array = unsafe { libc::malloc(size) }.cast::<*mut c_char>();
    if array.is_null() {
        return -libc::ENOMEM;
    }
    for (i, item) in items.iter().enumerate() {
        // SAFETY: i < items.len(), within the allocation of len + 1.
        unsafe { *array.add(i) = dup(item) };
    }
    // SAFETY: index len is the last slot of the allocation.
    unsafe { *array.add(items.len()) = std::ptr::null_mut() };
    // SAFETY: out is not null and writable.
    unsafe { *out = array };
    count
}

/// A C string argument as `&str`; `None` for null.
///
/// # Safety
/// `text`, if not null, is a NUL-terminated string.
unsafe fn arg<'a>(text: *const c_char) -> Result<Option<&'a str>, c_int> {
    if text.is_null() {
        return Ok(None);
    }
    // SAFETY: not null, and NUL-terminated by the caller's promise.
    let text = unsafe { CStr::from_ptr(text) };
    text.to_str().map(Some).map_err(|_| -EINVAL)
}

/// Writes a uid through `out`.
///
/// # Safety
/// `out`, if not null, points to a writable `uid_t`.
unsafe fn give_uid(out: *mut libc::uid_t, uid: u32) -> c_int {
    if !out.is_null() {
        // SAFETY: not null and writable.
        unsafe { *out = uid };
    }
    0
}

fn pid_of(pid: libc::pid_t) -> Result<u32, c_int> {
    u32::try_from(pid).map_err(|_| -EINVAL)
}

/// The process a pidfd refers to, from `/proc/self/fdinfo`.
fn pid_of_pidfd(fd: c_int) -> Result<u32, c_int> {
    let info =
        std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).map_err(|_| -libc::EBADF)?;
    info.lines()
        .find_map(|l| l.strip_prefix("Pid:"))
        .and_then(|p| p.trim().parse::<i64>().ok())
        .and_then(|p| u32::try_from(p).ok())
        .ok_or(-libc::ESRCH)
}

/// # Safety
/// `session`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_pid_get_session(pid: libc::pid_t, session: *mut *mut c_char) -> c_int {
    match pid_of(pid).and_then(|pid| query().session_of_pid(pid).map_err(|e| -e)) {
        // SAFETY: forwarded promise.
        Ok(id) => unsafe { give(session, &id) },
        Err(e) => e,
    }
}

/// # Safety
/// `session`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_pidfd_get_session(pidfd: c_int, session: *mut *mut c_char) -> c_int {
    match pid_of_pidfd(pidfd).and_then(|pid| query().session_of_pid(pid).map_err(|e| -e)) {
        // SAFETY: forwarded promise.
        Ok(id) => unsafe { give(session, &id) },
        Err(e) => e,
    }
}

/// # Safety
/// `uid`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_pid_get_owner_uid(pid: libc::pid_t, uid: *mut libc::uid_t) -> c_int {
    match pid_of(pid).and_then(|pid| query().owner_uid_of_pid(pid).map_err(|e| -e)) {
        // SAFETY: forwarded promise.
        Ok(owner) => unsafe { give_uid(uid, owner) },
        Err(e) => e,
    }
}

/// # Safety
/// `uid`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_pidfd_get_owner_uid(pidfd: c_int, uid: *mut libc::uid_t) -> c_int {
    match pid_of_pidfd(pidfd).and_then(|pid| query().owner_uid_of_pid(pid).map_err(|e| -e)) {
        // SAFETY: forwarded promise.
        Ok(owner) => unsafe { give_uid(uid, owner) },
        Err(e) => e,
    }
}

/// # Safety
/// `session`, if not null, is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_session_is_active(session: *const c_char) -> c_int {
    // SAFETY: forwarded promise.
    let session = match unsafe { arg(session) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    match query().session_is_active(session) {
        Ok(active) => c_int::from(active),
        Err(e) => -e,
    }
}

/// # Safety
/// `session`, if not null, is a NUL-terminated string; `state`, if not
/// null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_session_get_state(
    session: *const c_char,
    state: *mut *mut c_char,
) -> c_int {
    // SAFETY: forwarded promise.
    let session = match unsafe { arg(session) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    match query().session_state(session) {
        // SAFETY: forwarded promise.
        Ok(s) => unsafe { give(state, &s) },
        Err(e) => -e,
    }
}

/// # Safety
/// `session`, if not null, is a NUL-terminated string; `uid`, if not null,
/// is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_session_get_uid(
    session: *const c_char,
    uid: *mut libc::uid_t,
) -> c_int {
    // SAFETY: forwarded promise.
    let session = match unsafe { arg(session) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    match query().session_uid(session) {
        // SAFETY: forwarded promise.
        Ok(owner) => unsafe { give_uid(uid, owner) },
        Err(e) => -e,
    }
}

/// # Safety
/// `session`, if not null, is a NUL-terminated string; `seat`, if not
/// null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_session_get_seat(
    session: *const c_char,
    seat: *mut *mut c_char,
) -> c_int {
    // SAFETY: forwarded promise.
    let session = match unsafe { arg(session) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    match query().session_seat(session) {
        // SAFETY: forwarded promise.
        Ok(s) => unsafe { give(seat, &s) },
        Err(e) => -e,
    }
}

/// # Safety
/// `state`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_uid_get_state(uid: libc::uid_t, state: *mut *mut c_char) -> c_int {
    // SAFETY: forwarded promise.
    unsafe { give(state, &query().uid_state(uid)) }
}

/// # Safety
/// `session`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_uid_get_display(uid: libc::uid_t, session: *mut *mut c_char) -> c_int {
    match query().uid_display(uid) {
        // SAFETY: forwarded promise.
        Ok(id) => unsafe { give(session, &id) },
        Err(e) => -e,
    }
}

/// # Safety
/// `sessions`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_uid_get_sessions(
    uid: libc::uid_t,
    require_active: c_int,
    sessions: *mut *mut *mut c_char,
) -> c_int {
    let list = query().uid_sessions(uid, require_active > 0);
    // SAFETY: forwarded promise.
    unsafe { give_list(sessions, &list) }
}

/// # Safety
/// `seats`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_uid_get_seats(
    uid: libc::uid_t,
    require_active: c_int,
    seats: *mut *mut *mut c_char,
) -> c_int {
    let list = query().uid_seats(uid, require_active > 0);
    // SAFETY: forwarded promise.
    unsafe { give_list(seats, &list) }
}

/// # Safety
/// `category`, if not null, is a NUL-terminated string; `ret` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_login_monitor_new(
    category: *const c_char,
    ret: *mut *mut Monitor,
) -> c_int {
    if ret.is_null() {
        return -EINVAL;
    }
    // SAFETY: forwarded promise.
    let category = match unsafe { arg(category) } {
        Ok(c) => c,
        Err(e) => return e,
    };
    let dirs = match query().monitored(category) {
        Ok(dirs) => dirs,
        Err(e) => return -e,
    };
    // SAFETY: inotify_init1 with flags; the result is checked.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        return -std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
    }
    for dir in dirs {
        let _ = std::fs::create_dir_all(&dir);
        let Ok(path) = CString::new(dir.to_string_lossy().into_owned()) else {
            continue;
        };
        let mask = libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE | libc::IN_DELETE | libc::IN_CREATE;
        // SAFETY: a valid descriptor and a NUL-terminated path.
        if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } < 0 {
            let error = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            // SAFETY: our own descriptor.
            unsafe { libc::close(fd) };
            return -error;
        }
    }
    // SAFETY: ret is not null and writable; the value is fd + 1, as
    // sd-login encodes its own.
    unsafe { *ret = (fd as usize + 1) as *mut Monitor };
    0
}

fn monitor_fd(m: *mut Monitor) -> Option<c_int> {
    let value = m as usize;
    if value == 0 {
        None
    } else {
        c_int::try_from(value - 1).ok()
    }
}

/// # Safety
/// `m` is null or a monitor from `sd_login_monitor_new`, not yet unref'd.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_login_monitor_unref(m: *mut Monitor) -> *mut Monitor {
    if let Some(fd) = monitor_fd(m) {
        // SAFETY: the monitor's own descriptor, closed once.
        unsafe { libc::close(fd) };
    }
    std::ptr::null_mut()
}

/// Reads every event waiting: the caller has seen that something changed.
///
/// # Safety
/// `m` is a monitor from `sd_login_monitor_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_login_monitor_flush(m: *mut Monitor) -> c_int {
    let Some(fd) = monitor_fd(m) else {
        return -EINVAL;
    };
    let mut buffer = [0u8; 4096];
    loop {
        // SAFETY: reading into our own buffer, within its length.
        let n = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if n <= 0 {
            return 0;
        }
    }
}

/// # Safety
/// `m` is a monitor from `sd_login_monitor_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_login_monitor_get_fd(m: *mut Monitor) -> c_int {
    monitor_fd(m).unwrap_or(-EINVAL)
}

/// # Safety
/// `m` is a monitor from `sd_login_monitor_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_login_monitor_get_events(m: *mut Monitor) -> c_int {
    if monitor_fd(m).is_none() {
        return -EINVAL;
    }
    c_int::from(libc::POLLIN)
}

/// No timeout: the descriptor says when.
///
/// # Safety
/// `m` is a monitor; `timeout`, if not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_login_monitor_get_timeout(m: *mut Monitor, timeout: *mut u64) -> c_int {
    if monitor_fd(m).is_none() {
        return -EINVAL;
    }
    if !timeout.is_null() {
        // SAFETY: not null and writable.
        unsafe { *timeout = u64::MAX };
    }
    0
}

/// systemd's readiness protocol, which polkitd speaks: `state` sent to
/// `$NOTIFY_SOCKET` as one datagram — oxinit listens there for a `notify`
/// unit. 0 with no socket to send to, positive once sent.
///
/// # Safety
/// `state`, if not null, is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sd_notify(unset_environment: c_int, state: *const c_char) -> c_int {
    // SAFETY: forwarded promise.
    let state = match unsafe { arg(state) } {
        Ok(Some(s)) => s.to_owned(),
        Ok(None) => return -EINVAL,
        Err(e) => return e,
    };
    let socket = std::env::var("NOTIFY_SOCKET").ok();
    if unset_environment != 0 {
        // SAFETY: as sd_notify itself, which callers make before threads
        // that read the environment exist.
        unsafe { std::env::remove_var("NOTIFY_SOCKET") };
    }
    let Some(socket) = socket else {
        return 0;
    };
    let sent = (|| -> std::io::Result<()> {
        use std::os::linux::net::SocketAddrExt;
        let datagram = std::os::unix::net::UnixDatagram::unbound()?;
        let address = match socket.strip_prefix('@') {
            Some(name) => std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?,
            None => std::os::unix::net::SocketAddr::from_pathname(&socket)?,
        };
        datagram.send_to_addr(state.as_bytes(), &address)?;
        Ok(())
    })();
    match sent {
        Ok(()) => 1,
        Err(error) => -error.raw_os_error().unwrap_or(libc::EIO),
    }
}
