//! hide: hideOS's command line.
//!
//! ```text
//! hide install --payload FILE --disk DEVICE [--poweroff] [--user NAME --password PASS] [--encrypt PASS] [--swap MIB]
//! hide swap
//! hide setup [--root DIR]
//! hide update --image oci-archive:PATH | oci:DIR[:TAG]
//! hide status | rollback | boot-ok | gc
//! hide ext add oci-archive:PATH | list | remove NAME
//! hide daemon
//! poweroff | reboot | halt     (hide under those names)
//! ```
//!
//! `rebase` and `shell` come with H5.

#![forbid(unsafe_code)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod client;
#[cfg(target_os = "linux")]
mod crypt;
#[cfg(target_os = "linux")]
mod daemon;
#[cfg(target_os = "linux")]
mod deploy;
#[cfg(target_os = "linux")]
mod ext;
#[cfg(target_os = "linux")]
mod install;
#[cfg(target_os = "linux")]
mod installer;
#[cfg(target_os = "linux")]
mod power;
#[cfg(target_os = "linux")]
mod recovery_system;
#[cfg(target_os = "linux")]
mod setup;

use std::process::ExitCode;

fn main() -> ExitCode {
    // Called as poweroff, reboot or halt — the names elogind, and people,
    // reach for — ask oxinit to do it. oxinit takes those requests as
    // signals: see its ARCHITECTURE, "Shutdown".
    #[cfg(target_os = "linux")]
    if let Some(signal) = std::env::args()
        .next()
        .as_deref()
        .and_then(|a| a.rsplit('/').next())
        .and_then(shutdown_signal)
    {
        return match rustix::process::kill_process(rustix::process::Pid::INIT, signal) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("hide: asking init to shut down: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hide: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn shutdown_signal(name: &str) -> Option<rustix::process::Signal> {
    use rustix::process::Signal;
    match name {
        "poweroff" => Some(Signal::TERM),
        "reboot" => Some(Signal::INT),
        "halt" => Some(Signal::USR1),
        _ => None,
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(result) = through_daemon(args) {
        return result;
    }
    #[cfg(target_os = "linux")]
    let _lock = changes_the_system(args).then(lock).transpose()?;
    match args.first().map(String::as_str) {
        #[cfg(target_os = "linux")]
        Some("install") => install::run(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("installer") => installer::run(),
        #[cfg(target_os = "linux")]
        Some("recovery") => recovery_system::run(),
        #[cfg(target_os = "linux")]
        Some("setup") => setup::run(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("swap") => power::swap(),
        #[cfg(target_os = "linux")]
        Some("tpm-enroll") => crypt::tpm_enroll(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("update") => deploy::update(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("boot-ok") => deploy::boot_ok(),
        #[cfg(target_os = "linux")]
        Some("status") => deploy::status(),
        #[cfg(target_os = "linux")]
        Some("rollback") => deploy::rollback(),
        #[cfg(target_os = "linux")]
        Some("gc") => deploy::gc(),
        #[cfg(target_os = "linux")]
        Some("ext") => ext::run(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("daemon") => daemon::run(),
        Some("help" | "--help" | "-h") | None => {
            print!("{}", USAGE);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

/// The operations hideupd offers go through it when it is running; see
/// client.rs. A relative image path is made absolute first: the daemon's
/// working directory is not this one.
#[cfg(target_os = "linux")]
fn through_daemon(args: &[String]) -> Option<anyhow::Result<()>> {
    use client::Operation;
    let absolute = |image: &str| -> String {
        for transport in ["oci-archive:", "oci:"] {
            if let Some(path) = image.strip_prefix(transport)
                && !path.starts_with('/')
                && let Ok(cwd) = std::env::current_dir()
            {
                return format!("{transport}{}/{path}", cwd.display());
            }
        }
        image.to_owned()
    };
    let image;
    let operation = match args {
        [cmd, flag, given] if cmd == "update" && flag == "--image" => {
            image = absolute(given);
            Operation::Update(&image)
        }
        [cmd] if cmd == "rollback" => Operation::Rollback,
        [cmd] if cmd == "gc" => Operation::Collect,
        [cmd, sub, given] if cmd == "ext" && sub == "add" => {
            image = absolute(given);
            Operation::AddExtension(&image)
        }
        [cmd, sub, name] if cmd == "ext" && sub == "remove" => Operation::RemoveExtension(name),
        _ => return None,
    };
    client::through_daemon(operation)
}

#[cfg(target_os = "linux")]
fn changes_the_system(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("update" | "rollback" | "gc" | "boot-ok") => true,
        Some("ext") => args.get(1).is_some_and(|sub| sub != "list"),
        _ => false,
    }
}

/// One change to the system at a time, whether hideupd or a shell makes
/// it: an exclusive lock on a file in /run, held until the command ends.
#[cfg(target_os = "linux")]
fn lock() -> anyhow::Result<rustix::fd::OwnedFd> {
    use anyhow::Context;
    use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
    let fd = open(
        "/run/hide.lock",
        OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .context("opening /run/hide.lock")?;
    match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(fd),
        Err(rustix::io::Errno::WOULDBLOCK) => {
            anyhow::bail!("another update operation is running")
        }
        Err(error) => Err(error).context("locking /run/hide.lock"),
    }
}

const USAGE: &str = "usage: hide <command>

    install --payload FILE --disk DEVICE [--poweroff] [--user NAME --password PASS] [--encrypt PASS] [--swap MIB]
        Install hideOS on DEVICE, erasing it, from a payload hideforge built,
        and create the first account, an administrator. The swap file is as
        large as this machine's memory, or MIB.

    installer
        Ask which disk, a passphrase and the first account, and install from
        the installer medium this machine booted. The installer image's
        init.

    recovery
        The recovery system's init: choose which system starts next, open
        a shell with the disk mounted, restart, turn off.

    setup [--root DIR]
        Create /etc/machine-id, the system users in sysusers.d and the paths
        in tmpfiles.d that do not exist yet. Run at every boot.

    tpm-enroll [--if-missing]
        Seal the encrypted root's key to this machine's TPM, for this boot
        chain, so the disk opens without a passphrase. --if-missing, at
        boot: only when nothing is sealed yet.

    swap
        Turn the swap file on, and record where a hibernated system will be
        found. Run at every boot.

    update --image oci-archive:PATH | oci:DIR[:TAG]
        Stage a new system from the OCI image hideforge built. It starts at
        the next boot, with three attempts before the machine goes back.

    status
        The deployments on this machine, in the order they boot.

    rollback
        Boot the previous deployment next.

    boot-ok
        Mark the deployment that booted as good. Run at the end of boot.

    ext add oci-archive:PATH | list | remove NAME
        System extensions hideOS signed: added to the store, merged over
        /usr from the next boot of the system each was built for.

    gc
        Remove from the store what no deployment on the ESP uses. An update
        does this by itself.

    daemon
        hideupd: offer update, rollback, gc and ext add|remove on the system
        bus as os.hide.Update1, to root and to whom polkit allows. While it
        runs, those commands go through it.
";
