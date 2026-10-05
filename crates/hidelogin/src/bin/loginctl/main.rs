//! loginctl: the part of elogind's command hideOS uses, as a client of
//! hidelogin on the system bus. COSMIC's shortcuts run `loginctl suspend`
//! and `loginctl lock-session`; the rest is for a person at a terminal.

#![deny(unsafe_code)]

use std::process::ExitCode;

const USAGE: &str = "usage: loginctl list-sessions | lock-session [ID] | unlock-session [ID] \
| suspend | hibernate | poweroff | reboot";

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("loginctl runs on Linux");
    ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("loginctl: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("loginctl: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
async fn run(args: &[String]) -> Result<(), String> {
    use zbus::Connection;
    use zbus::zvariant::OwnedObjectPath;

    const NAME: &str = "org.freedesktop.login1";
    const PATH: &str = "/org/freedesktop/login1";
    const MANAGER: &str = "org.freedesktop.login1.Manager";

    let bus = Connection::system().await.map_err(|e| e.to_string())?;
    let manager = |method: &'static str| {
        let bus = bus.clone();
        async move {
            // Interactive: polkit may ask for a password where the rules
            // want one, as it does for elogind's loginctl.
            bus.call_method(Some(NAME), PATH, Some(MANAGER), method, &(true,))
                .await
                .map(drop)
                .map_err(|e| e.to_string())
        }
    };
    // A session named, or the caller's own: logind's "auto".
    let session = |id: Option<&String>| -> Result<String, String> {
        match id {
            None => Ok(format!("{PATH}/session/auto")),
            Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()) => {
                Ok(format!("{PATH}/session/{}", escape(id)))
            }
            Some(id) => Err(format!("no session {id}")),
        }
    };
    let on_session = |path: String, method: &'static str| {
        let bus = bus.clone();
        async move {
            bus.call_method(
                Some(NAME),
                path.as_str(),
                Some("org.freedesktop.login1.Session"),
                method,
                &(),
            )
            .await
            .map(drop)
            .map_err(|e| e.to_string())
        }
    };

    match (
        args.first().map(String::as_str),
        args.get(1..).unwrap_or_default(),
    ) {
        (Some("list-sessions"), []) => {
            let reply = bus
                .call_method(Some(NAME), PATH, Some(MANAGER), "ListSessions", &())
                .await
                .map_err(|e| e.to_string())?;
            let sessions: Vec<(String, u32, String, String, OwnedObjectPath)> =
                reply.body().deserialize().map_err(|e| e.to_string())?;
            println!("{:>7} {:>6} {:<16} {:<8}", "SESSION", "UID", "USER", "SEAT");
            for (id, uid, user, seat, _) in &sessions {
                println!("{id:>7} {uid:>6} {user:<16} {seat:<8}");
            }
            println!("\n{} sessions listed.", sessions.len());
            Ok(())
        }
        (Some("lock-session"), rest) if rest.len() <= 1 => {
            on_session(session(rest.first())?, "Lock").await
        }
        (Some("unlock-session"), rest) if rest.len() <= 1 => {
            on_session(session(rest.first())?, "Unlock").await
        }
        (Some("suspend"), []) => manager("Suspend").await,
        (Some("hibernate"), []) => manager("Hibernate").await,
        (Some("poweroff"), []) => manager("PowerOff").await,
        (Some("reboot"), []) => manager("Reboot").await,
        _ => Err(USAGE.to_owned()),
    }
}

/// A session id in an object path, as logind escapes it: `1` is `_31`.
#[cfg(target_os = "linux")]
fn escape(id: &str) -> String {
    let mut escaped = String::new();
    for (i, byte) in id.bytes().enumerate() {
        if byte.is_ascii_alphabetic() || (byte.is_ascii_digit() && i > 0) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("_{byte:02x}"));
        }
    }
    escaped
}
