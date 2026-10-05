//! hidelogin: seats, sessions and power for hideOS, in place of elogind.
//! See ARCHITECTURE.md, "hidelogin"; the policy is the library's.

#![deny(unsafe_code)]

#[cfg(target_os = "linux")]
mod buttons;
#[cfg(target_os = "linux")]
mod daemon;
#[cfg(target_os = "linux")]
mod login1;
#[cfg(target_os = "linux")]
mod pam_server;
#[cfg(target_os = "linux")]
mod power;
#[cfg(target_os = "linux")]
mod seatd_server;
#[cfg(target_os = "linux")]
mod shutdown;
#[cfg(target_os = "linux")]
mod sys;
#[cfg(target_os = "linux")]
mod vt;

use std::process::ExitCode;

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("hidelogin runs on Linux");
    ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hidelogin: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run());
    // Not a plain drop: dropping the runtime waits for every blocking
    // thread it started, and one blocked on a read held a stopped
    // hidelogin until oxinit's stop-sec ran out.
    runtime.shutdown_timeout(std::time::Duration::from_millis(500));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hidelogin: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
async fn run() -> anyhow::Result<()> {
    use std::sync::{Arc, Mutex};

    use anyhow::Context;

    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let (usr, etc) = (
        read("/usr/lib/hidelogin/logind.conf"),
        read("/etc/hidelogin/logind.conf"),
    );
    let config = hidelogin::conf::Config::read(&[&usr, &etc]).context("logind.conf")?;

    // A restart starts over: sessions from before are not this daemon's.
    // The directories stay and only their files go: sd-login's monitors in
    // polkit and NetworkManager, started before, watch these directories,
    // and a watch on a removed one is never woken again. All three exist
    // from the start for the same monitors.
    for dir in ["sessions", "users", "seats"] {
        let dir = std::path::Path::new(hidelogin::session::STATE).join(dir);
        std::fs::create_dir_all(&dir)?;
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    std::fs::create_dir_all(format!("/sys/fs/cgroup/{}", hidelogin::session::SLICE))
        .context("creating the sessions' cgroup slice")?;

    let shared = Arc::new(Mutex::new(daemon::Daemon::new(config)));
    daemon::lock(&shared).write_state();
    login1::serve(shared.clone())
        .await
        .context("on the system bus")?;
    println!("hidelogin: serving");
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = terminate.recv() => {
            shutdown::end_sessions().await;
            Ok(())
        }
        r = pam_server::serve(shared.clone()) => r.context("PAM's socket"),
        r = seatd_server::serve(shared.clone()) => r.context("seatd's socket"),
        r = vt::signals(shared.clone()) => r.context("VT signals"),
        r = vt::follow(shared.clone()) => r.context("the VT shown"),
        r = buttons::serve(shared.clone()) => r.context("the lid and the power key"),
    }
}
