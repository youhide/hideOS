//! The build sandbox. See `docs/HIDEFORGE.md#the-sandbox`.
//!
//! Three processes, because Linux puts a new PID namespace's first process at
//! PID 1 only for *children* of whoever unshared it:
//!
//! ```text
//! hideforge build              spawns, captures output to the log
//! └─ hideforge __sandbox       unshares the namespaces, waits
//!    └─ hideforge __sandbox-init   PID 1: mounts, pivots, runs bash, reaps
//!       └─ bash build script
//! ```
//!
//! Everything here runs in a privileged builder container, as root. No user
//! namespace: the builder already is the isolation boundary from the host, and
//! these namespaces are about hermeticity — what a build can see — not about
//! containing a hostile one.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use hideforge_recipe::Environment;
use rustix::fs::Timespec;
use rustix::fs::{AtFlags, CWD, Timestamps};
use rustix::mount::{
    FsMountFlags, FsOpenFlags, MountAttrFlags, MountFlags, MountPropagationFlags, MoveMountFlags,
    UnmountFlags,
};
use rustix::process::WaitOptions;

use crate::output;
use crate::sys;

/// Everything the sandbox needs, passed from `build` to `__sandbox-init` on
/// the command line, so that what a build ran with is visible in `ps`.
#[derive(Debug, Clone)]
pub struct Spec {
    pub environment: Environment,
    /// Mount point for the overlay in a `target` build. Unused in `host`
    /// builds, which mount it at `/sysroot`.
    pub root: PathBuf,
    pub upper: PathBuf,
    pub overlay_work: PathBuf,
    /// Top first. The skeleton, if any, goes last.
    pub lowers: Vec<PathBuf>,
    pub src: PathBuf,
    pub home: PathBuf,
    /// The script, as the sandbox sees it.
    pub script: String,
    pub vars: Vec<(String, String)>,
}

impl Spec {
    fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "--environment".to_owned(),
            match self.environment {
                Environment::Host => "host",
                Environment::Target => "target",
            }
            .to_owned(),
        ];
        let mut path = |flag: &str, value: &Path| {
            args.push(flag.to_owned());
            args.push(value.display().to_string());
        };
        path("--root", &self.root);
        path("--upper", &self.upper);
        path("--overlay-work", &self.overlay_work);
        path("--src", &self.src);
        path("--home", &self.home);
        for lower in &self.lowers {
            path("--lower", lower);
        }
        args.push("--script".to_owned());
        args.push(self.script.clone());
        for (key, value) in &self.vars {
            args.push("--var".to_owned());
            args.push(format!("{key}={value}"));
        }
        args
    }

    fn from_args(args: &[String]) -> Result<Spec> {
        let mut spec = Spec {
            environment: Environment::Target,
            root: PathBuf::new(),
            upper: PathBuf::new(),
            overlay_work: PathBuf::new(),
            lowers: Vec::new(),
            src: PathBuf::new(),
            home: PathBuf::new(),
            script: String::new(),
            vars: Vec::new(),
        };
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let value = iter.next().ok_or_else(|| anyhow!("{flag} needs a value"))?;
            match flag.as_str() {
                "--environment" => {
                    spec.environment = match value.as_str() {
                        "host" => Environment::Host,
                        "target" => Environment::Target,
                        other => bail!("unknown environment {other}"),
                    }
                }
                "--root" => spec.root = value.into(),
                "--upper" => spec.upper = value.into(),
                "--overlay-work" => spec.overlay_work = value.into(),
                "--src" => spec.src = value.into(),
                "--home" => spec.home = value.into(),
                "--lower" => spec.lowers.push(value.into()),
                "--script" => spec.script = value.clone(),
                "--var" => {
                    let (key, val) = value
                        .split_once('=')
                        .ok_or_else(|| anyhow!("--var {value}: expected KEY=VALUE"))?;
                    spec.vars.push((key.to_owned(), val.to_owned()));
                }
                other => bail!("unknown sandbox argument {other}"),
            }
        }
        if spec.lowers.is_empty() {
            bail!("an overlay needs at least one lower layer");
        }
        Ok(spec)
    }
}

/// Runs a build in a sandbox, with its stdout and stderr going to `log`.
pub fn run(spec: &Spec, log: &Path) -> Result<ExitStatus> {
    let log_file = fs::File::create(log).with_context(|| log.display().to_string())?;
    let exe = std::env::current_exe().context("finding hideforge's own executable")?;
    Command::new(exe)
        .arg("__sandbox")
        .args(spec.to_args())
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .status()
        .context("starting the sandbox")
}

/// `hideforge __sandbox`: new namespaces, then the PID 1 that lives in them.
pub fn outer(args: &[String]) -> Result<i32> {
    sys::unshare_sandbox_namespaces().context("unshare")?;
    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("__sandbox-init")
        .args(args)
        .status()
        .context("starting the sandbox's PID 1")?;
    Ok(exit_code(status))
}

/// `hideforge __sandbox-init`: PID 1 of the build's PID namespace.
pub fn init(args: &[String]) -> Result<i32> {
    let spec = Spec::from_args(args)?;

    // Nothing mounted from here on may propagate back to the builder.
    rustix::mount::mount_change(
        "/",
        MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
    )
    .context("making / private")?;

    let path = match spec.environment {
        Environment::Target => {
            enter_target_root(&spec)?;
            "/tools/bin:/usr/bin:/usr/sbin:/bin:/sbin"
        }
        Environment::Host => {
            enter_host_root(&spec)?;
            "/sysroot/tools/bin:/usr/local/bin:/usr/bin:/usr/sbin:/bin:/sbin"
        }
    };
    rustix::system::sethostname(b"hideforge").context("sethostname")?;

    let mut command = Command::new("/bin/bash");
    command
        .args(["-euo", "pipefail", &spec.script])
        .current_dir("/build/src")
        .env_clear()
        .env("PATH", path)
        .env("HOME", "/build/home")
        // C, not C.UTF-8: C.UTF-8 is a locale that has to be installed, and
        // a bootstrap root does not have it yet. C always exists.
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        // The build runs as root in its namespace. Autoconf's checks for
        // that exist to stop people installing onto a live system as root,
        // which is not what is happening here.
        .env("FORCE_UNSAFE_CONFIGURE", "1")
        .stdin(Stdio::null());
    for (key, value) in &spec.vars {
        command.env(key, value);
    }
    let child = command
        .spawn()
        .context("starting /bin/bash in the sandbox")?;
    let script_pid = child.id();

    // PID 1 reaps everything. The build's own status is the script's; other
    // processes it orphaned are reaped and forgotten, and anything still
    // running when the script exits dies with the namespace when we return.
    loop {
        match rustix::process::wait(WaitOptions::empty()) {
            Ok(Some((pid, status))) if pid.as_raw_nonzero().get().unsigned_abs() == script_pid => {
                return Ok(match (status.exit_status(), status.terminating_signal()) {
                    (Some(code), _) => code,
                    (None, Some(signal)) => 128 + signal,
                    (None, None) => 1,
                });
            }
            Ok(_) => {}
            Err(error) => bail!("waiting for the build script: {error}"),
        }
    }
}

/// `/` becomes the overlay; the builder disappears.
fn enter_target_root(spec: &Spec) -> Result<()> {
    let root = &spec.root;
    mount_overlay(spec, root)?;

    mount_fs(
        "proc",
        &root.join("proc"),
        "proc",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
    )?;
    mount_sysfs(&root.join("sys"))?;
    mount_fs(
        "tmpfs",
        &root.join("tmp"),
        "tmpfs",
        MountFlags::NOSUID | MountFlags::NODEV,
    )?;
    fs::set_permissions(root.join("tmp"), fs::Permissions::from_mode(0o1777))?;

    // A minimal /dev: the handful of nodes builds use, bound from the
    // builder's, on a tmpfs that is not part of the output.
    let dev = root.join("dev");
    mount_fs(
        "tmpfs",
        &dev,
        "tmpfs",
        MountFlags::NOSUID | MountFlags::NOEXEC,
    )?;
    for node in ["null", "zero", "full", "random", "urandom", "tty"] {
        let target = dev.join(node);
        fs::write(&target, b"")?;
        rustix::mount::mount_bind(Path::new("/dev").join(node), &target)
            .with_context(|| format!("binding /dev/{node}"))?;
    }
    for (name, target) in [
        ("fd", "/proc/self/fd"),
        ("stdin", "/proc/self/fd/0"),
        ("stdout", "/proc/self/fd/1"),
        ("stderr", "/proc/self/fd/2"),
    ] {
        symlink(target, dev.join(name))?;
    }
    fs::create_dir(dev.join("shm"))?;
    mount_fs(
        "tmpfs",
        &dev.join("shm"),
        "tmpfs",
        MountFlags::NOSUID | MountFlags::NODEV,
    )?;

    rustix::mount::mount_bind(&spec.src, root.join("build/src")).context("binding sources")?;
    rustix::mount::mount_bind(&spec.home, root.join("build/home")).context("binding home")?;

    std::env::set_current_dir(root)?;
    rustix::process::pivot_root(".", ".old").context("pivot_root")?;
    std::env::set_current_dir("/")?;
    rustix::mount::unmount("/.old", UnmountFlags::DETACH).context("detaching the old root")?;
    Ok(())
}

/// `/` stays the builder, read-only; the overlay goes on `/sysroot`; the work
/// volume and the checkout are hidden.
fn enter_host_root(spec: &Spec) -> Result<()> {
    mount_overlay(spec, Path::new("/sysroot"))?;
    mount_fs(
        "proc",
        Path::new("/proc"),
        "proc",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
    )?;
    mount_sysfs(Path::new("/sys"))?;
    rustix::mount::mount_bind(&spec.src, "/build/src").context("binding sources")?;
    rustix::mount::mount_bind(&spec.home, "/build/home").context("binding home")?;
    // After the binds, which come from /work: covering /work does not
    // unmount what was bound out of it.
    for hidden in ["/work", "/src"] {
        if Path::new(hidden).is_dir() {
            mount_fs("tmpfs", Path::new(hidden), "tmpfs", MountFlags::empty())?;
        }
    }
    mount_fs(
        "tmpfs",
        Path::new("/tmp"),
        "tmpfs",
        MountFlags::NOSUID | MountFlags::NODEV,
    )?;
    fs::set_permissions("/tmp", fs::Permissions::from_mode(0o1777))?;
    rustix::mount::mount_remount("/", MountFlags::BIND | MountFlags::RDONLY, c"")
        .context("making the builder's root read-only")?;
    Ok(())
}

/// The overlay, through the new mount API: one `lowerdir+` per layer, so the
/// number of dependencies is not limited by the 4 KiB `mount(2)` option
/// string.
fn mount_overlay(spec: &Spec, target: &Path) -> Result<()> {
    let context = || format!("mounting the overlay on {}", target.display());
    let fs = rustix::mount::fsopen("overlay", FsOpenFlags::FSOPEN_CLOEXEC).with_context(context)?;
    for lower in &spec.lowers {
        rustix::mount::fsconfig_set_string(&fs, "lowerdir+", lower)
            .with_context(|| format!("lower layer {}", lower.display()))?;
    }
    rustix::mount::fsconfig_set_string(&fs, "upperdir", &spec.upper).with_context(context)?;
    rustix::mount::fsconfig_set_string(&fs, "workdir", &spec.overlay_work).with_context(context)?;
    // Plain copy-up only. With metacopy, a changed file's upper copy would
    // hold only metadata and point at its lower layer through an xattr, and
    // the output would be a file that does not exist outside this overlay.
    for (key, value) in [
        ("metacopy", "off"),
        ("redirect_dir", "off"),
        ("index", "off"),
    ] {
        rustix::mount::fsconfig_set_string(&fs, key, value).with_context(context)?;
    }
    rustix::mount::fsconfig_create(&fs).with_context(context)?;
    let mount = rustix::mount::fsmount(&fs, FsMountFlags::FSMOUNT_CLOEXEC, MountAttrFlags::empty())
        .with_context(context)?;
    rustix::mount::move_mount(
        &mount,
        "",
        CWD,
        target,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .with_context(context)?;
    Ok(())
}

/// A fresh sysfs, read-only. Fresh because sysfs shows the network devices of
/// the namespace that *mounted* it: the builder's `/sys` lists the builder's
/// interfaces even from inside the sandbox's empty network namespace, which
/// is a build being told about a network it cannot reach.
fn mount_sysfs(target: &Path) -> Result<()> {
    mount_fs(
        "sysfs",
        target,
        "sysfs",
        MountFlags::RDONLY | MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
    )
}

fn mount_fs(source: &str, target: &Path, fstype: &str, flags: MountFlags) -> Result<()> {
    rustix::mount::mount(source, target, fstype, flags, None)
        .with_context(|| format!("mounting {fstype} on {}", target.display()))
}

/// Sets every timestamp newer than `epoch` to `epoch`, symlinks included.
pub fn clamp_mtimes(dir: &Path, epoch: u64) -> Result<()> {
    let epoch_secs = i64::try_from(epoch).unwrap_or(i64::MAX);
    let stamp = Timespec {
        tv_sec: epoch_secs,
        tv_nsec: 0,
    };
    let times = Timestamps {
        last_access: stamp,
        last_modification: stamp,
    };
    let mut paths: Vec<PathBuf> = output::walk(dir)?
        .into_iter()
        .filter(|(_, meta)| {
            use std::os::unix::fs::MetadataExt;
            meta.mtime() > epoch_secs
        })
        .map(|(relative, _)| dir.join(relative))
        .collect();
    paths.push(dir.to_path_buf());
    for path in paths {
        rustix::fs::utimensat(CWD, &path, &times, AtFlags::SYMLINK_NOFOLLOW)
            .with_context(|| format!("setting times on {}", path.display()))?;
    }
    Ok(())
}

/// Removes overlay's bookkeeping xattrs from an output, which mean something
/// only inside the overlay that wrote them.
pub fn strip_overlay_xattrs(dir: &Path) -> Result<()> {
    let mut paths: Vec<PathBuf> = output::walk(dir)?
        .into_iter()
        .map(|(relative, _)| dir.join(relative))
        .collect();
    paths.push(dir.to_path_buf());
    let mut buffer = vec![0u8; 64 * 1024];
    for path in paths {
        let length = match rustix::fs::llistxattr(&path, &mut buffer[..]) {
            Ok(length) => length,
            Err(rustix::io::Errno::NOTSUP) => continue,
            Err(error) => return Err(error).with_context(|| path.display().to_string()),
        };
        let names = buffer.get(..length).unwrap_or_default();
        for name in names
            .split(|b| *b == 0)
            .filter(|n| n.starts_with(b"trusted.overlay."))
        {
            let name = std::str::from_utf8(name).map_err(|_| anyhow!("non-UTF-8 xattr name"))?;
            rustix::fs::lremovexattr(&path, name)
                .with_context(|| format!("removing {name} from {}", path.display()))?;
        }
    }
    Ok(())
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}
