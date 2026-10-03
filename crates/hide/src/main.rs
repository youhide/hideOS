//! hide: hideOS's command line.
//!
//! ```text
//! hide install --payload FILE --disk DEVICE [--poweroff] [--user NAME --password PASS] [--encrypt PASS] [--swap MIB]
//! hide swap
//! hide setup [--root DIR]
//! hide update --image oci-archive:PATH | oci:DIR[:TAG]
//! hide status | rollback | boot-ok | gc
//! poweroff | reboot | halt     (hide under those names)
//! ```
//!
//! `rebase` and `shell` come with H5.

#![forbid(unsafe_code)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod crypt;
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
    match args.first().map(String::as_str) {
        #[cfg(target_os = "linux")]
        Some("install") => install::run(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("installer") => installer::run(),
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
        Some("help" | "--help" | "-h") | None => {
            print!("{}", USAGE);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command `{other}`\n\n{USAGE}"),
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
";
