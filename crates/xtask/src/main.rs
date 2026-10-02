//! Development automation for hideOS.
//!
//! ```text
//! cargo xtask doctor                  what this machine can and cannot do yet
//! cargo xtask builder build           build the Linux builder image
//! cargo xtask builder shell           a shell inside it
//! cargo xtask builder run -- CMD...   one command inside it
//! cargo xtask firmware-smoke [--arch x86_64|aarch64]
//!                                     boot UEFI firmware in QEMU and check it
//!                                     reaches boot device selection
//! ```
//!
//! This runs on the developer's machine, not on the boot path. The rules for
//! boot-path crates do not apply here.

#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("doctor") => doctor(),
        Some("builder") => builder(args.get(1..).unwrap_or_default()),
        Some("firmware-smoke") => firmware_smoke(args.get(1..).unwrap_or_default()),
        Some("help" | "--help" | "-h") | None => {
            print!("{}", usage());
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{}", usage())),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage: cargo xtask <command>

    doctor                        check this machine's tools and firmware
    builder build                 build the Linux builder image
    builder shell                 open a shell in the builder
    builder run -- CMD [ARG...]   run one command in the builder
    firmware-smoke [--arch ARCH]  boot UEFI firmware in QEMU (x86_64, aarch64)
"
}

// ---------------------------------------------------------------------------
// Architectures and firmware

/// A target hideOS is built and booted for.
#[derive(Clone, Copy)]
struct Arch {
    name: &'static str,
    qemu: &'static str,
    /// Machine and CPU. `virt` is the board QEMU invented for aarch64 guests;
    /// `q35` is the x86 chipset with PCIe, which is what real UEFI machines
    /// look like. `max` gives every feature the accelerator can offer, and is
    /// accepted by KVM, HVF and TCG alike, so one argument list works
    /// everywhere.
    machine: &'static [&'static str],
    /// Places firmware is installed, as (code, variable store template)
    /// pairs, in the order they are tried.
    firmware: &'static [(&'static str, &'static str)],
    /// How long the firmware smoke test waits. A foreign architecture runs
    /// through QEMU's JIT with nothing to accelerate it.
    timeout: Duration,
}

const ARCHES: &[Arch] = &[
    Arch {
        name: "x86_64",
        qemu: "qemu-system-x86_64",
        machine: &["-machine", "q35", "-cpu", "max"],
        firmware: &[
            // Debian and Ubuntu, `ovmf`.
            (
                "/usr/share/OVMF/OVMF_CODE_4M.fd",
                "/usr/share/OVMF/OVMF_VARS_4M.fd",
            ),
            // Fedora, `edk2-ovmf`.
            (
                "/usr/share/edk2/ovmf/OVMF_CODE.fd",
                "/usr/share/edk2/ovmf/OVMF_VARS.fd",
            ),
            // Arch, `edk2-ovmf`.
            (
                "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
                "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
            ),
            // Homebrew's `qemu` ships its own. The x86_64 build shares the
            // i386 variable store, as QEMU's own firmware descriptors say.
            (
                "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
                "/opt/homebrew/share/qemu/edk2-i386-vars.fd",
            ),
            (
                "/usr/local/share/qemu/edk2-x86_64-code.fd",
                "/usr/local/share/qemu/edk2-i386-vars.fd",
            ),
        ],
        timeout: Duration::from_secs(90),
    },
    Arch {
        name: "aarch64",
        qemu: "qemu-system-aarch64",
        machine: &["-machine", "virt", "-cpu", "max"],
        firmware: &[
            // Debian and Ubuntu, `qemu-efi-aarch64`.
            (
                "/usr/share/AAVMF/AAVMF_CODE.fd",
                "/usr/share/AAVMF/AAVMF_VARS.fd",
            ),
            // Fedora, `edk2-aarch64`.
            (
                "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
                "/usr/share/edk2/aarch64/vars-template-pflash.raw",
            ),
            // Arch, `edk2-aarch64`.
            (
                "/usr/share/edk2/aarch64/QEMU_CODE.fd",
                "/usr/share/edk2/aarch64/QEMU_VARS.fd",
            ),
            (
                "/opt/homebrew/share/qemu/edk2-aarch64-code.fd",
                "/opt/homebrew/share/qemu/edk2-arm-vars.fd",
            ),
            (
                "/usr/local/share/qemu/edk2-aarch64-code.fd",
                "/usr/local/share/qemu/edk2-arm-vars.fd",
            ),
        ],
        timeout: Duration::from_secs(300),
    },
];

/// `--arch NAME`, then `$HIDEOS_ARCH`, then the host's own.
fn find_arch(args: &[String]) -> Result<Arch, String> {
    let wanted = match flag(args, "--arch")? {
        Some(name) => name.to_owned(),
        None => env::var("HIDEOS_ARCH").unwrap_or_else(|_| env::consts::ARCH.to_owned()),
    };
    ARCHES
        .iter()
        .find(|arch| arch.name == wanted)
        .copied()
        .ok_or_else(|| {
            let known: Vec<&str> = ARCHES.iter().map(|arch| arch.name).collect();
            format!("unknown --arch `{wanted}`; known: {}", known.join(", "))
        })
}

fn find_firmware(arch: Arch) -> Option<(PathBuf, PathBuf)> {
    arch.firmware
        .iter()
        .map(|(code, vars)| (PathBuf::from(code), PathBuf::from(vars)))
        .find(|(code, vars)| code.is_file() && vars.is_file())
}

/// The accelerators worth trying, best first. QEMU takes `-accel` more than
/// once and uses the first that initialises, so asking for one this host does
/// not have is a warning on stderr and a fallback to TCG, not a failure.
fn accelerators(arch: Arch) -> Vec<&'static str> {
    let mut accels = Vec::new();
    if arch.name == env::consts::ARCH {
        match env::consts::OS {
            "linux" if Path::new("/dev/kvm").exists() => accels.push("kvm"),
            "macos" => accels.push("hvf"),
            _ => {}
        }
    }
    accels.push("tcg");
    accels
}

// ---------------------------------------------------------------------------
// doctor

/// One line of `doctor` output.
enum Check {
    Ok(String),
    /// Missing, and something cannot be done without it.
    Missing(String),
    /// Worth knowing, not a failure.
    Note(String),
}

fn doctor() -> Result<(), String> {
    let mut checks = Vec::new();

    for arch in ARCHES {
        checks.push(if on_path(arch.qemu) {
            Check::Ok(format!("{} found", arch.qemu))
        } else {
            Check::Missing(format!("{} not on PATH", arch.qemu))
        });
        checks.push(match find_firmware(*arch) {
            Some((code, _)) => {
                Check::Ok(format!("{} UEFI firmware: {}", arch.name, code.display()))
            }
            None => Check::Missing(format!(
                "{} UEFI firmware not found in any of the usual places; install `ovmf` / \
                 `qemu-efi-aarch64` (Debian), `edk2-ovmf` / `edk2-aarch64` (Fedora, Arch), or \
                 Homebrew's `qemu`",
                arch.name
            )),
        });
    }

    let runtime = container_runtime();
    checks.push(match &runtime {
        Some(runtime) if daemon_reachable(runtime) => {
            Check::Ok(format!("{runtime} is installed and its daemon answers"))
        }
        Some(runtime) => Check::Missing(format!(
            "{runtime} is installed but its daemon does not answer; start it \
             (Docker Desktop on macOS) before `cargo xtask builder`"
        )),
        None => Check::Missing("neither docker nor podman is on PATH".to_owned()),
    });

    checks.push(match case_insensitive_here() {
        Ok(true) => Check::Note(
            "this checkout is on a case-insensitive filesystem. Fine for the repository, \
             fatal for building Linux, whose source has files differing only in case. \
             Builds therefore run in the builder's `hideos-work` volume, never in the \
             checkout."
                .to_owned(),
        ),
        Ok(false) => Check::Ok("this checkout is on a case-sensitive filesystem".to_owned()),
        Err(error) => Check::Note(format!(
            "could not probe filesystem case sensitivity: {error}"
        )),
    });

    let mut missing = 0;
    for check in &checks {
        match check {
            Check::Ok(line) => println!("  ok    {line}"),
            Check::Missing(line) => {
                missing += 1;
                println!("  MISS  {line}");
            }
            Check::Note(line) => println!("  note  {line}"),
        }
    }

    if missing == 0 {
        println!("\nEverything H0 needs is here.");
        Ok(())
    } else {
        Err(format!("{missing} thing(s) missing; see above"))
    }
}

fn on_path(program: &str) -> bool {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Writes two names that differ only in case and asks whether the second one
/// landed on the first. Asking the filesystem is the only reliable test:
/// APFS, HFS+, NTFS and ext4 with `casefold` all answer differently, and none
/// advertise it anywhere portable.
fn case_insensitive_here() -> Result<bool, String> {
    let dir = workspace_root()?.join("target").join("case-probe");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let lower = dir.join("probe");
    let upper = dir.join("PROBE");
    let _ = fs::remove_file(&upper);
    fs::write(&lower, b"").map_err(|e| format!("{}: {e}", lower.display()))?;
    let insensitive = upper.exists();
    let _ = fs::remove_dir_all(&dir);
    Ok(insensitive)
}

// ---------------------------------------------------------------------------
// builder

/// The builder image. One tag, rebuilt in place: the Containerfile is the
/// version, and `builder build` is cheap when nothing in it changed.
const BUILDER_IMAGE: &str = "hideos-builder:dev";

/// Where builds happen. A named volume lives on the container runtime's own
/// Linux filesystem, which is case-sensitive and fast, unlike a bind mount of
/// a macOS checkout, which is neither.
const WORK_VOLUME: &str = "hideos-work";

/// Crates downloaded by cargo inside the builder, kept across runs.
const REGISTRY_VOLUME: &str = "hideos-cargo-registry";

fn builder(args: &[String]) -> Result<(), String> {
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    let root = workspace_root()?;

    match args.first().map(String::as_str) {
        Some("build") => {
            let context = root.join("tools").join("builder");
            run(Command::new(&runtime)
                .arg("build")
                .args(["--tag", BUILDER_IMAGE])
                .arg("--file")
                .arg(context.join("Containerfile"))
                .arg(&context))
        }
        Some("shell") => run(builder_command(&runtime, &root, true).arg("bash")),
        Some("run") => {
            let command = match args.get(1).map(String::as_str) {
                Some("--") => args.get(2..).unwrap_or_default(),
                _ => args.get(1..).unwrap_or_default(),
            };
            if command.is_empty() {
                return Err("usage: cargo xtask builder run -- CMD [ARG...]".to_owned());
            }
            run(builder_command(&runtime, &root, false).args(command))
        }
        _ => Err("usage: cargo xtask builder build|shell|run -- CMD [ARG...]".to_owned()),
    }
}

fn builder_command(runtime: &str, root: &Path, interactive: bool) -> Command {
    let mut command = Command::new(runtime);
    command.args(["run", "--rm"]);
    if interactive {
        command.arg("-it");
    }
    command
        .arg("--volume")
        .arg(format!("{}:/src", root.display()))
        .args(["--volume", &format!("{WORK_VOLUME}:/work")])
        .args([
            "--volume",
            &format!("{REGISTRY_VOLUME}:/usr/local/cargo/registry"),
        ])
        // Build output goes to the volume, not into the checkout's `target/`,
        // which on macOS is a case-insensitive bind mount and much slower.
        .args(["--env", "CARGO_TARGET_DIR=/work/target"])
        .args(["--workdir", "/src"]);
    // KVM, where the host has it. Docker Desktop on macOS never does: QEMU
    // inside the builder runs on TCG there, and `firmware-smoke` on the host
    // is the faster path.
    if Path::new("/dev/kvm").exists() {
        command.args(["--device", "/dev/kvm"]);
    }
    command.arg(BUILDER_IMAGE);
    command
}

/// `$HIDEOS_CONTAINER`, then docker, then podman.
fn container_runtime() -> Option<String> {
    if let Ok(runtime) = env::var("HIDEOS_CONTAINER") {
        return Some(runtime);
    }
    ["docker", "podman"]
        .into_iter()
        .find(|runtime| on_path(runtime))
        .map(str::to_owned)
}

fn daemon_reachable(runtime: &str) -> bool {
    Command::new(runtime)
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// firmware-smoke

/// What the serial console prints once the firmware has initialised the
/// platform and moved on to choosing something to boot. With no disk and no
/// network there is nothing to choose, which is the point: reaching this line
/// proves QEMU, the firmware and the variable store work together, and that
/// is everything H2's disk boot will stand on.
const BDS_MARKERS: &[&str] = &["BdsDxe", "UEFI Interactive Shell", "Shell>"];

fn firmware_smoke(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let (code, vars_template) = find_firmware(arch).ok_or_else(|| {
        format!(
            "no {} UEFI firmware found; `cargo xtask doctor` says where it was looked for",
            arch.name
        )
    })?;

    let dir = workspace_root()?.join("target").join("firmware-smoke");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // The variable store is written by the firmware, so every run gets a
    // fresh copy. A store left over from an earlier run is state the test did
    // not set up.
    let vars = dir.join(format!("{}-vars.fd", arch.name));
    fs::copy(&vars_template, &vars).map_err(|e| format!("{}: {e}", vars.display()))?;

    let log = dir.join(format!("{}.log", arch.name));
    let _ = fs::remove_file(&log);

    let mut command = Command::new(arch.qemu);
    for accel in accelerators(arch) {
        command.args(["-accel", accel]);
    }
    command
        .args(arch.machine)
        .args(["-m", "512", "-smp", "2"])
        .args(["-display", "none", "-monitor", "none", "-nic", "none"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            code.display()
        ))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=1,file={}",
            vars.display()
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    println!(
        "booting {} firmware {} (timeout {}s)",
        arch.name,
        code.display(),
        arch.timeout.as_secs()
    );
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", arch.qemu))?;

    let outcome = loop {
        let serial = match fs::read(&log) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => break Err(format!("{}: {error}", log.display())),
        };
        if let Some(marker) = BDS_MARKERS.iter().find(|marker| serial.contains(*marker)) {
            break Ok(format!(
                "reached boot device selection (`{marker}`) in {:.1}s",
                started.elapsed().as_secs_f64()
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break Err(format!(
                    "{} exited ({status}) before the firmware reached boot device selection",
                    arch.qemu
                ));
            }
            Ok(None) => {}
            Err(error) => break Err(format!("waiting on {}: {error}", arch.qemu)),
        }
        if started.elapsed() > arch.timeout {
            break Err(format!(
                "no sign of boot device selection after {}s",
                arch.timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(200));
    };

    let _ = child.kill();
    let output = child.wait_with_output().ok();

    match outcome {
        Ok(message) => {
            println!("{}: {message}", arch.name);
            Ok(())
        }
        Err(message) => {
            if let Some(output) = output.filter(|output| !output.stderr.is_empty()) {
                eprintln!(
                    "{} said:\n{}",
                    arch.qemu,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(format!(
                "{}: {message}; serial log in {}",
                arch.name,
                log.display()
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// helpers

/// `--name VALUE`, if present.
fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, String> {
    match args.iter().position(|arg| arg == name) {
        None => Ok(None),
        Some(at) => args
            .get(at + 1)
            .map(|value| Some(value.as_str()))
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

/// The directory holding the workspace `Cargo.toml`. xtask's own manifest is
/// two levels below it.
fn workspace_root() -> Result<PathBuf, String> {
    let manifest = env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set; run this through `cargo xtask`")?;
    Path::new(&manifest)
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{manifest} has no workspace root above it"))
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("could not run {command:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        // Pass the child's own exit code through where there is one, so
        // `builder run -- false` behaves like `false`.
        process::exit(status.code().unwrap_or(1));
    }
}
