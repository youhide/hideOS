//! `hide shell [NAME]`: a shell in a container that shares the person's
//! home, display and sound, toolbox-style — where compilers, language
//! toolchains and package managers go, since nothing is installed into the
//! sealed system. See ARCHITECTURE.md, "The shell".
//!
//! Rootless Podman, as the person running it. The container is made the
//! first time, from the image in /usr/lib/hide/shell.conf (overridden in
//! /etc/hide/shell.conf), and kept: what is installed in it stays until
//! `hide shell --remove NAME`.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

const DEFAULT_NAME: &str = "default";

pub fn run(args: &[String]) -> Result<()> {
    match args {
        [] => enter(DEFAULT_NAME),
        [name] if !name.starts_with('-') => enter(name),
        [flag, name] if flag == "--remove" => remove(name),
        _ => bail!("usage: hide shell [NAME] | --remove NAME"),
    }
}

fn container(name: &str) -> Result<String> {
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "`{name}` is not a shell name: letters, digits, - and _"
    );
    Ok(format!("hide-{name}"))
}

fn enter(name: &str) -> Result<()> {
    ensure!(
        !rustix::process::getuid().is_root(),
        "hide shell is for a person, not root: the container is theirs"
    );
    let container = container(name)?;
    if !exists(&container)? {
        create(&container)?;
    }
    podman(&["start", &container])?;

    let pwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let mut exec = Command::new("podman");
    exec.args(["exec", "-it", "--workdir"]).arg(&pwd);
    for var in [
        "TERM",
        "COLORTERM",
        "LANG",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "DBUS_SESSION_BUS_ADDRESS",
        "DISPLAY",
    ] {
        if std::env::var_os(var).is_some() {
            exec.args(["-e", var]);
        }
    }
    // The person's shell if the image has it, else bash, else sh.
    exec.args([
        &container,
        "sh",
        "-c",
        "for s in zsh bash sh; do command -v $s >/dev/null && exec $s -l; done",
    ]);
    Err(exec.exec()).context("running podman")
}

fn create(container: &str) -> Result<()> {
    let image = image()?;
    let home = std::env::var("HOME").context("HOME is not set")?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let user = std::env::var("USER").context("USER is not set")?;
    eprintln!("hide shell: making {container} from {image}");
    let mut args: Vec<String> = [
        "create",
        "--name",
        container,
        "--hostname",
        container,
        "--label",
        "os.hide.shell=1",
        // The person is the same person inside, with their groups — the
        // devices they may open stay theirs.
        "--userns=keep-id",
        "--group-add=keep-groups",
        // The host's network: no translation, and servers a program in the
        // container starts are on the host's localhost.
        "--network=host",
        "--init",
        "--ulimit=host",
        "--tz=local",
        "--device",
        "/dev/dri",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // The home, and the session's runtime directory — the display, sound
    // and session bus sockets are in it.
    for dir in [&home, &runtime] {
        args.push("--volume".to_owned());
        args.push(format!("{dir}:{dir}"));
    }
    if Path::new("/tmp/.X11-unix").exists() {
        args.push("--volume".to_owned());
        args.push("/tmp/.X11-unix:/tmp/.X11-unix".to_owned());
    }
    args.push(image);
    args.extend(["sleep".to_owned(), "infinity".to_owned()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    podman(&args)?;
    podman(&["start", container])?;
    // Administering what is inside is the person's: sudo without a
    // password, in the container only.
    let sudoers = format!("{user} ALL=(ALL) NOPASSWD: ALL\n");
    let status = Command::new("podman")
        .args(["exec", "--user", "root", "-i", container, "sh", "-c"])
        .arg("mkdir -p /etc/sudoers.d && cat > /etc/sudoers.d/hide-shell && chmod 440 /etc/sudoers.d/hide-shell")
        .stdin(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(sudoers.as_bytes())?;
            }
            child.wait()
        })
        .context("running podman")?;
    if !status.success() {
        eprintln!("hide shell: the image has no sudo; it is still usable");
    }
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let container = container(name)?;
    podman(&["rm", "--force", &container])
}

fn exists(container: &str) -> Result<bool> {
    Ok(Command::new("podman")
        .args(["container", "exists", container])
        .status()
        .context("running podman: is the Workstation's podman installed?")?
        .success())
}

/// The image from shell.conf: `image = REFERENCE`.
fn image() -> Result<String> {
    let mut image = None;
    for path in ["/usr/lib/hide/shell.conf", "/etc/hide/shell.conf"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("image").map(str::trim_start)
                && let Some(value) = value.strip_prefix('=')
            {
                image = Some(value.trim().to_owned());
            }
        }
    }
    image.context("no image in /usr/lib/hide/shell.conf")
}

fn podman(args: &[&str]) -> Result<()> {
    let status = Command::new("podman")
        .args(args)
        .status()
        .context("running podman")?;
    ensure!(status.success(), "podman {} failed", args.join(" "));
    Ok(())
}
