//! Development automation for hideOS.
//!
//! ```text
//! cargo xtask doctor                  what this machine can and cannot do yet
//! cargo xtask builder build           build the Linux builder image
//! cargo xtask builder shell           a shell inside it
//! cargo xtask builder run -- CMD...   one command inside it
//! cargo xtask forge -- ARG...        hideforge, in the builder
//! cargo xtask forge-selftest          check the sandbox's guarantees
//! cargo xtask image [--arch ARCH]     build hideOS Minimal into target/images
//! cargo xtask boot [--arch] [--test]  boot it in QEMU
//! cargo xtask screenshot [--arch]     boot it, type commands, save a PNG
//! cargo xtask publish-site            push site/ to the gh-pages branch
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
        Some("forge") => forge(args.get(1..).unwrap_or_default()),
        Some("forge-selftest") => forge_selftest(),
        Some("image") => image(args.get(1..).unwrap_or_default()),
        Some("boot") => boot(args.get(1..).unwrap_or_default()),
        Some("screenshot") => screenshot(args.get(1..).unwrap_or_default()),
        Some("publish-site") => publish_site(),
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
    forge -- ARG...               run hideforge in the builder
    forge-selftest                check the sandbox's guarantees
    image [--arch ARCH]           build hideOS Minimal into target/images
    boot [--arch ARCH] [--test]   boot it in QEMU; --test waits for the banner
    screenshot [--arch ARCH]      boot it, type a few commands, save a PNG
    publish-site                  push site/ and the screenshot to gh-pages
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
    /// The serial port the kernel is told to use. Wrong, and a boot prints
    /// nothing at all, with nothing to say why.
    console: &'static str,
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
        console: "ttyS0",
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
        console: "ttyAMA0",
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
        .args(["--env", "HIDEFORGE_WORK=/work"])
        .args(["--workdir", "/src"])
        // hideforge's sandbox creates namespaces and mounts overlays. The
        // builder is the isolation boundary from the host; inside it,
        // namespaces are for hermeticity, and they need the privilege.
        .arg("--privileged");
    // Host-environment recipes hash the builder they ran in.
    if let Some(id) = builder_image_id(runtime) {
        command.args(["--env", &format!("HIDEFORGE_HOST_ID={id}")]);
    }
    // KVM, where the host has it. Docker Desktop on macOS never does: QEMU
    // inside the builder runs on TCG there, and `firmware-smoke` on the host
    // is the faster path.
    if Path::new("/dev/kvm").exists() {
        command.args(["--device", "/dev/kvm"]);
    }
    command.arg(BUILDER_IMAGE);
    command
}

fn builder_image_id(runtime: &str) -> Option<String> {
    let output = Command::new(runtime)
        .args(["image", "inspect", "--format", "{{.Id}}", BUILDER_IMAGE])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let id = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !id.is_empty()).then_some(id)
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

/// `cargo xtask forge -- build NAME`: hideforge, built in release mode inside
/// the builder and run there. Release because it hashes and walks trees of
/// tens of thousands of files, and a debug build makes that the slow part.
fn forge(args: &[String]) -> Result<(), String> {
    let args = match args.first().map(String::as_str) {
        Some("--") => args.get(1..).unwrap_or_default(),
        _ => args,
    };
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    let root = workspace_root()?;
    run(builder_command(&runtime, &root, false)
        .args([
            "cargo",
            "run",
            "--quiet",
            "--release",
            "--package",
            "hideforge",
            "--",
        ])
        .args(args))
}

/// Builds the fixtures in `crates/hideforge/tests/recipes` in a scratch work
/// directory and checks each guarantee the sandbox makes. The fixtures check
/// isolation from the inside, and fail their own build if any check fails;
/// this checks the outcomes from the outside.
fn forge_selftest() -> Result<(), String> {
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    let root = workspace_root()?;
    let script = r#"
set -u
W=/work/selftest
rm -rf "$W"
# Its own target directory, so a self-test never rebuilds the hideforge a
# real build in another container is running.
export CARGO_TARGET_DIR=/work/target-selftest
F="cargo run --quiet --release --package hideforge -- --recipes crates/hideforge/tests/recipes --work $W"
fail() { echo "FAIL: $*"; exit 1; }

$F build stage1-target || fail "the host and target fixtures should build"
out=$(ls -d $W/store/*-stage1-target-1)
# usr/bin is there, empty: overlay copied the directory up when ls was
# deleted from it, and the whiteout recording that is dropped.
listing=$(cd "$out" && find . -mindepth 1 | sort | tr '\n' ' ')
[ "$listing" = "./usr ./usr/bin ./usr/lib ./usr/lib/marker ./usr/share ./usr/share/selftest ./usr/share/selftest/result " ] \
    || fail "stage1-target's output should be exactly what it made: $listing"
[ "$(cat "$out/usr/lib/marker")" = "replaced in stage 1" ] || fail "stage 1 should replace stage 0's file"
echo "ok    host build: no network, read-only builder, hidden /work and /src"
echo "ok    target build: pivoted root, inputs visible, builder gone"
echo "ok    output is exactly what the build created"
echo "ok    a later stage replaces and deletes an earlier stage's files"

$F build stage1-target | grep -q cached || fail "a second build should be cached"
echo "ok    unchanged inputs are not rebuilt"

$F build stage1-replaces 2>&1 | grep -q "replaced inherited /usr/lib/stage1-marker" \
    || fail "replacing a same-stage file should be refused, naming it"
echo "ok    replacing a file from the same stage is refused"

$F build stage1-fails 2>&1 | grep -q "exit status: 3" || fail "the script's exit status should be reported"
ls -d $W/store/*-stage1-fails-1 >/dev/null 2>&1 && fail "a failed build should leave no store path"
echo "ok    a failed build reports its status and leaves no store path"

[ -z "$(ls -A $W/build 2>/dev/null)" ] || fail "build directories left behind: $(ls $W/build)"
echo "ok    no build directories left behind"
"#;
    run(builder_command(&runtime, &root, false).args(["bash", "-c", script]))
}

// ---------------------------------------------------------------------------
// image and boot

/// The edition `image`, `boot` and `screenshot` work with. Workstation joins
/// it in H5.
const EDITION: &str = "minimal";

/// Where `image` puts an architecture's kernel and initramfs: in the
/// checkout, so QEMU on the host can read them. Two files, so the
/// case-insensitivity that rules out building here does not matter.
fn image_dir(arch: Arch) -> Result<PathBuf, String> {
    Ok(workspace_root()?
        .join("target")
        .join("images")
        .join(format!("{EDITION}-{}", arch.name)))
}

fn image(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    let root = workspace_root()?;
    let output = format!("/src/target/images/{EDITION}-{}", arch.name);
    run(builder_command(&runtime, &root, false)
        .args([
            "cargo",
            "run",
            "--quiet",
            "--release",
            "--package",
            "hideforge",
            "--",
        ])
        .args(["--arch", arch.name, "image", EDITION, "--kernel", "linux"])
        .args(["--output", &output]))
}

/// What the hideOS banner unit prints once oxinit has started it: proof that
/// the kernel booted, oxinit is PID 1, read its units, and ran a program on
/// the glibc userspace.
const BOOT_MARKER: &str = "hideOS: booted on";

fn boot(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let test = args.iter().any(|a| a == "--test");
    let dir = image_dir(arch)?;
    let kernel = dir.join("vmlinuz");
    let initrd = dir.join("initramfs.cpio");
    if !kernel.is_file() || !initrd.is_file() {
        return Err(format!(
            "no image in {}; run `cargo xtask image --arch {}` first",
            dir.display(),
            arch.name
        ));
    }

    let mut command = Command::new(arch.qemu);
    for accel in accelerators(arch) {
        command.args(["-accel", accel]);
    }
    command
        .args(arch.machine)
        .args(["-m", "2048", "-smp", "2", "-no-reboot", "-nic", "none"])
        .arg("-kernel")
        .arg(&kernel)
        .arg("-initrd")
        .arg(&initrd)
        // rdinit, not init: in an initramfs the kernel runs rdinit, and the
        // default /init does not exist in a hideOS root. panic=-1: reboot at
        // once on a panic, which -no-reboot turns into QEMU exiting, so a
        // failed boot ends instead of hanging.
        .arg("-append")
        .arg(format!(
            "console={} rdinit=/usr/bin/oxinit panic=-1",
            arch.console
        ));

    if !test {
        println!("booting hideOS {} (quit QEMU with Ctrl-A X)", arch.name);
        command.arg("-nographic");
        return run(&mut command);
    }

    let log = dir.join("serial.log");
    let _ = fs::remove_file(&log);
    command
        .args(["-display", "none", "-monitor", "none"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    println!(
        "booting hideOS {} (timeout {}s)",
        arch.name,
        arch.timeout.as_secs()
    );
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", arch.qemu))?;
    let outcome = loop {
        let serial = fs::read(&log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if let Some(line) = serial.lines().find(|l| l.contains(BOOT_MARKER)) {
            break Ok(format!(
                "{} in {:.1}s",
                line.trim(),
                started.elapsed().as_secs_f64()
            ));
        }
        if let Ok(Some(status)) = child.try_wait() {
            break Err(format!("QEMU exited ({status}) before the banner"));
        }
        if started.elapsed() > arch.timeout {
            break Err(format!("no banner after {}s", arch.timeout.as_secs()));
        }
        thread::sleep(Duration::from_millis(200));
    };
    let _ = child.kill();
    let _ = child.wait();
    match outcome {
        Ok(message) => {
            println!("{}: {message}", arch.name);
            Ok(())
        }
        Err(message) => Err(format!(
            "{}: {message}; serial log in {}",
            arch.name,
            log.display()
        )),
    }
}

/// What `screenshot` types at the console, one command per entry.
const SCREENSHOT_COMMANDS: &[&str] = &[
    "clear",
    "cat /etc/os-release",
    "uname -sr",
    "oxctl list",
    "ls /",
];

/// Boots the image with a display, types [`SCREENSHOT_COMMANDS`] at the
/// console through QEMU's monitor, and saves what the screen shows. A real
/// boot and real output, not a mock-up: the README's pictures are this.
fn screenshot(args: &[String]) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let arch = find_arch(args)?;
    let dir = image_dir(arch)?;
    let kernel = dir.join("vmlinuz");
    let initrd = dir.join("initramfs.cpio");
    if !kernel.is_file() || !initrd.is_file() {
        return Err(format!(
            "no image in {}; run `cargo xtask image` first",
            dir.display()
        ));
    }
    let socket = dir.join("monitor.sock");
    let _ = fs::remove_file(&socket);
    let png = dir.join("screenshot.png");
    let _ = fs::remove_file(&png);

    let mut command = Command::new(arch.qemu);
    for accel in accelerators(arch) {
        command.args(["-accel", accel]);
    }
    let video: &[&str] = if arch.name == "aarch64" {
        &["-device", "ramfb"]
    } else {
        &["-vga", "std"]
    };
    command
        .args(arch.machine)
        .args(["-m", "2048", "-smp", "2", "-no-reboot", "-nic", "none"])
        .args(video)
        .args(["-display", "none", "-serial", "null"])
        .arg("-monitor")
        .arg(format!("unix:{},server,nowait", socket.display()))
        .arg("-kernel")
        .arg(&kernel)
        .arg("-initrd")
        .arg(&initrd)
        // The last console= is /dev/console, where oxinit puts the login
        // shell: the screen, this time. quiet keeps the kernel's own
        // messages off it.
        .arg("-append")
        .arg(format!(
            "console={} console=tty0 rdinit=/usr/bin/oxinit panic=-1 quiet",
            arch.console
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", arch.qemu))?;

    let result = (|| -> Result<(), String> {
        let started = Instant::now();
        let mut monitor = loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => break stream,
                Err(_) if started.elapsed() < Duration::from_secs(10) => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("QEMU monitor: {e}")),
            }
        };
        monitor
            .set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(|e| e.to_string())?;
        let mut send = |line: &str| -> Result<(), String> {
            monitor
                .write_all(format!("{line}\n").as_bytes())
                .map_err(|e| format!("QEMU monitor: {e}"))?;
            // Drain the echo, so the socket never fills.
            let mut sink = [0u8; 4096];
            while let Ok(n) = monitor.read(&mut sink) {
                if n == 0 {
                    break;
                }
            }
            Ok(())
        };

        // Long enough to boot to the shell, with time to spare on TCG.
        let boot_wait = if accelerators(arch).len() > 1 { 12 } else { 60 };
        thread::sleep(Duration::from_secs(boot_wait));
        for line in SCREENSHOT_COMMANDS {
            for c in line.chars() {
                send(&format!("sendkey {}", qcode(c)?))?;
                thread::sleep(Duration::from_millis(15));
            }
            send("sendkey ret")?;
            thread::sleep(Duration::from_millis(1200));
        }
        send(&format!("screendump {} -f png", png.display()))?;
        thread::sleep(Duration::from_secs(2));
        if png.is_file() {
            Ok(())
        } else {
            Err("QEMU did not write the screenshot".to_owned())
        }
    })();

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&socket);
    result?;
    println!("{}: {}", arch.name, png.display());
    Ok(())
}

/// Publishes `site/` and the latest x86_64 screenshot to the `gh-pages`
/// branch, which GitHub Pages serves. By hand, on purpose: nothing deploys
/// on push. The branch holds only the built site, one commit per publish,
/// each naming the main commit it came from.
fn publish_site() -> Result<(), String> {
    let root = workspace_root()?;
    let screenshot = root
        .join("target/images")
        .join(format!("{EDITION}-x86_64"))
        .join("screenshot.png");
    if !screenshot.is_file() {
        return Err(format!(
            "no {}; run `cargo xtask screenshot` first",
            screenshot.display()
        ));
    }
    let script = r#"
set -eu
root="$1"; shot="$2"
source_commit=$(git -C "$root" rev-parse --short HEAD)
tree=$(mktemp -d)
trap 'git -C "$root" worktree remove --force "$tree" >/dev/null 2>&1 || true' EXIT
if git -C "$root" ls-remote --exit-code --heads origin gh-pages >/dev/null 2>&1; then
    git -C "$root" fetch -q origin gh-pages
    git -C "$root" worktree add -q "$tree" -B gh-pages origin/gh-pages
else
    git -C "$root" worktree add -q --detach "$tree"
    git -C "$tree" checkout -q --orphan gh-pages
fi
git -C "$tree" rm -rq --ignore-unmatch . >/dev/null
cp "$root/site/index.html" "$tree/index.html"
cp "$shot" "$tree/screenshot.png"
# Plain files, not a Jekyll site.
touch "$tree/.nojekyll"
git -C "$tree" add -A
if git -C "$tree" diff --cached --quiet; then
    echo "site unchanged"
    exit 0
fi
git -C "$tree" commit -q -m "site: from $source_commit"
git -C "$tree" push -q origin gh-pages
echo "published from $source_commit"
"#;
    run(Command::new("sh")
        .args(["-c", script, "publish-site"])
        .arg(&root)
        .arg(&screenshot))
}

/// The QEMU key name that types `c` on a US keyboard.
fn qcode(c: char) -> Result<String, String> {
    let plain = |name: &str| Ok(name.to_owned());
    let shifted = |name: &str| Ok(format!("shift-{name}"));
    match c {
        'a'..='z' | '0'..='9' => plain(&c.to_string()),
        'A'..='Z' => shifted(&c.to_ascii_lowercase().to_string()),
        ' ' => plain("spc"),
        '-' => plain("minus"),
        '=' => plain("equal"),
        '/' => plain("slash"),
        '.' => plain("dot"),
        ',' => plain("comma"),
        ';' => plain("semicolon"),
        '\'' => plain("apostrophe"),
        '\\' => plain("backslash"),
        '_' => shifted("minus"),
        '+' => shifted("equal"),
        '|' => shifted("backslash"),
        ':' => shifted("semicolon"),
        '"' => shifted("apostrophe"),
        '~' => shifted("grave_accent"),
        '>' => shifted("dot"),
        '<' => shifted("comma"),
        '?' => shifted("slash"),
        '*' => shifted("8"),
        '$' => shifted("4"),
        other => Err(format!("no key for {other:?}")),
    }
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
