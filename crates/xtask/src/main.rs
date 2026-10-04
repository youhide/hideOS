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

mod wiki;

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
        Some("install") => install(args.get(1..).unwrap_or_default()),
        Some("boot") => boot(args.get(1..).unwrap_or_default()),
        Some("seal-test") => seal_test(args.get(1..).unwrap_or_default()),
        Some("update-test") => update_test(args.get(1..).unwrap_or_default()),
        Some("net-test") => net_test(args.get(1..).unwrap_or_default()),
        Some("power-test") => power_test(args.get(1..).unwrap_or_default()),
        Some("crypt-test") => crypt_test(args.get(1..).unwrap_or_default()),
        Some("sysext-test") => sysext_test(args.get(1..).unwrap_or_default()),
        Some("desktop-test") => desktop_test(args.get(1..).unwrap_or_default()),
        Some("desktop-shell") => desktop_shell(args.get(1..).unwrap_or_default()),
        Some("setup-test") => setup_test(args.get(1..).unwrap_or_default()),
        Some("hw-test") => hw_test(args.get(1..).unwrap_or_default()),
        Some("registry-test") => registry_test(args.get(1..).unwrap_or_default()),
        Some("secureboot-keys") => secureboot_keys().map(|_| ()),
        Some("secureboot-test") => secureboot_test(args.get(1..).unwrap_or_default()),
        Some("installer") => installer(args.get(1..).unwrap_or_default()).map(|_| ()),
        Some("installer-test") => installer_test(args.get(1..).unwrap_or_default()),
        Some("screenshot") => screenshot(args.get(1..).unwrap_or_default()),
        Some("hideboot-screenshot") => hideboot_screenshot(args.get(1..).unwrap_or_default()),
        Some("publish-site") => publish_site(),
        Some("publish") => publish(args.get(1..).unwrap_or_default()),
        Some("wiki") => wiki::run(),
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
    image [--arch ARCH] [--edition E] [--image-version N]
                                  build an edition (minimal, workstation)
                                  into target/images; the version defaults
                                  to the number of commits on HEAD
    install [--arch ARCH] [--edition E]
                                  install it on target/images/.../disk.raw
    boot [--arch ARCH] [--edition E] [--test] [--disk] [--secure-boot]
                                  boot it in QEMU; --test waits for the
                                  banner; --disk boots disk.raw through UEFI
                                  (Workstation always does); --secure-boot
                                  with firmware that enforces signatures
    seal-test [--arch ARCH]       install Minimal on a scratch disk and try to
                                  break the seal, under Secure Boot: write
                                  /usr, tamper with an object, swap the
                                  image, change the kernel image
    update-test [--arch ARCH]     update Minimal N to N+1 on scratch disks: a
                                  good update, a bad one that has to roll
                                  back by itself, and a power cut after each
                                  step of an update
    net-test [--arch ARCH]        install Minimal and check that its network
                                  comes up: DHCP, DNS, iwd, logs
    power-test [--arch ARCH]      install Minimal, suspend it and wake it,
                                  hibernate it and resume it
    installer [--arch ARCH] [--edition E]
                                  an installer medium: a disk image with the
                                  installer and E's payload, for a USB stick
    installer-test [--arch ARCH] [--edition E]
                                  boot the installer beside an empty disk,
                                  answer it, and boot what it installed; the
                                  Workstation's asks only about the disk and
                                  starts first-boot setup
    sysext-test [--arch ARCH]     system extensions: refused unsigned, left out
                                  when built for another image, merged over
                                  /usr when signed for this one
    crypt-test [--arch ARCH]      install Minimal encrypted, and open it by
                                  passphrase, by TPM, and by passphrase again
                                  when the boot chain changes (needs swtpm)
    secureboot-keys               make hideOS's development Secure Boot keys and
                                  the signed lists the installer enrolls
    secureboot-test [--arch ARCH] from a firmware with no keys: enroll hideOS's,
                                  then boot under them and refuse a changed UKI
    registry-test [--arch ARCH]   update Minimal N to N+1 from a registry, by
                                  channel alone: plain `hide update`
    hw-test [--arch ARCH]         install Minimal and check what real hardware
                                  needs: udev loads drivers as modules, the
                                  firmware is there, microcode leads the initrd
    desktop-test [--arch ARCH]    install the Workstation and check what it adds:
                                  hideupd on the bus, polkit's actions, Flathub
                                  reached and verified, a sandbox as a user
    setup-test [--arch ARCH]      install the Workstation encrypted and with no
                                  account, as the installer does: first-boot
                                  setup comes up, takes each choice as the
                                  greeter, makes the account, gives the disk
                                  its passphrase and a recovery key, then the
                                  login screen lets the person in
    desktop-shell [--arch ARCH]   boot the disk desktop-test installed, log in at the
                                  greeter, then run each file put in the image
                                  directory's desktop-shell/in as a command on
                                  the serial console, its output to .../out;
                                  `screenshot` saves the screen, `quit` ends
    screenshot [--arch ARCH] [--edition E] [--login] [--then STEPS]
                                  boot it and save a PNG of the screen;
                                  --login logs in at the greeter first;
                                  --then key:meta_l-t;type:TEXT;wait:5;mouse:640 400
    hideboot-screenshot [--arch ARCH] [--no-build] [--manager FILE] [--display WxH]
                                  install Minimal, give its ESP an entry in
                                  each state and the recovery system, and
                                  save PNGs of hideBoot's menu
    publish --edition E --channel C [--arch ARCH]
                                  push the built image to
                                  ghcr.io/youhide/hideos:E-C, with gh's token
    publish-site                  push site/ and the screenshots to gh-pages
    wiki                          render docs/wiki into site/wiki
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

/// An edition of hideOS. See ARCHITECTURE.md, "Editions".
#[derive(Clone, Copy, PartialEq, Eq)]
struct Edition {
    /// The image recipe, and the name of its directory in target/images.
    name: &'static str,
    /// Whether it also boots from RAM. Minimal does, and has to: it is what
    /// `install` boots to run the installer. Workstation is too big to be
    /// worth it; it boots from its disk.
    ram: bool,
    /// Guest memory.
    memory: &'static str,
}

const MINIMAL: Edition = Edition {
    name: "minimal",
    ram: true,
    memory: "2048",
};

const EDITIONS: &[Edition] = &[
    MINIMAL,
    Edition {
        name: "workstation",
        ram: false,
        memory: "4096",
    },
];

fn find_edition(args: &[String]) -> Result<Edition, String> {
    let wanted = flag(args, "--edition")?.unwrap_or(MINIMAL.name);
    EDITIONS
        .iter()
        .find(|e| e.name == wanted)
        .copied()
        .ok_or_else(|| {
            let known: Vec<&str> = EDITIONS.iter().map(|e| e.name).collect();
            format!("unknown --edition `{wanted}`; known: {}", known.join(", "))
        })
}

/// Where `image` puts an edition's files for an architecture: in the
/// checkout, so QEMU on the host can read them. A few large files, so the
/// case-insensitivity that rules out building here does not matter.
fn image_dir(edition: Edition, arch: Arch) -> Result<PathBuf, String> {
    Ok(workspace_root()?
        .join("target")
        .join("images")
        .join(format!("{}-{}", edition.name, arch.name)))
}

fn image(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let edition = find_edition(args)?;
    let version = match flag(args, "--image-version")? {
        Some(v) => v
            .parse()
            .map_err(|_| format!("--image-version `{v}` is not a whole number"))?,
        None => image_version()?,
    };
    build_image(arch, edition, version, "", None)
}

/// The version an image built from this checkout gets: the number of
/// commits behind it, which only goes up along main. Not a build counter:
/// two builds of the same commit are the same version, and, being the same
/// inputs, the same image.
fn image_version() -> Result<u64, String> {
    let out = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(workspace_root()?)
        .output()
        .map_err(|e| format!("running git: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| "git rev-list --count HEAD gave no number".to_owned())
}

/// Builds `edition` with `version` into target/images/EDITION-ARCH, or a
/// subdirectory of it named `sub`.
fn build_image(
    arch: Arch,
    edition: Edition,
    version: u64,
    sub: &str,
    cmdline: Option<&str>,
) -> Result<(), String> {
    build_image_signed(arch, edition, version, sub, cmdline, DEV_KEYS)
}

/// `build_image`, signed with the keys in `keys`, a directory in the
/// builder: hideOS's own development keys for `secureboot-test`.
fn build_image_signed(
    arch: Arch,
    edition: Edition,
    version: u64,
    sub: &str,
    cmdline: Option<&str>,
    keys: &str,
) -> Result<(), String> {
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    let root = workspace_root()?;
    let mut output = format!("/src/target/images/{}-{}", edition.name, arch.name);
    if !sub.is_empty() {
        output = format!("{output}/{sub}");
    }
    let version = version.to_string();
    // The serial console last, so that it is /dev/console and the tests
    // can read and type: production images keep the screen there.
    let cmdline = match cmdline {
        Some(extra) => format!("console={} {extra}", arch.console),
        None => format!("console={}", arch.console),
    };
    run(builder_command(&runtime, &root, false).args(["sh", "-c", DEV_KEYS_SCRIPT]))?;
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
        .args([
            "--arch",
            arch.name,
            "image",
            edition.name,
            "--kernel",
            "linux",
        ])
        .args(["--payload", "--initrd", "hidestage", "--sign", keys])
        .args(["--boot-manager", "hideboot"])
        // Early microcode: x86 only, where the kernel reads it.
        .args(if arch.name == "x86_64" {
            &["--microcode", "microcode"][..]
        } else {
            &[][..]
        })
        .args(["--image-version", &version])
        .args(["--cmdline", &cmdline])
        // Minimal, which runs from memory, is also the installer.
        .args(if edition.ram {
            &["--installer"][..]
        } else {
            &["--no-initramfs"][..]
        })
        .args(["--output", &output]))
}

/// What the hideOS banner unit prints once oxinit has started it: proof that
/// the kernel booted, oxinit is PID 1, read its units, and ran a program on
/// the glibc userspace.
const BOOT_MARKER: &str = "hideOS: booted on";

/// The QEMU every boot starts from: the architecture's machine, the best
/// accelerator, no network, and `-no-reboot`, so that a guest rebooting —
/// after a panic, or hidestage giving up — ends the run instead of looping.
fn qemu(arch: Arch, edition: Edition) -> Command {
    qemu_with(arch, edition, false)
}

/// `qemu`, without macOS's hypervisor when `no_hvf`: for firmware that
/// needs System Management Mode — x86 Secure Boot keeps its variable store
/// there, and hvf has none — and for a machine with an emulated TPM, whose
/// firmware stalls under hvf before it prints a line. The guest then runs
/// on QEMU's own CPU emulation, slower; KVM has neither problem.
fn qemu_with(arch: Arch, edition: Edition, no_hvf: bool) -> Command {
    let mut command = Command::new(arch.qemu);
    // A watchdog, which hidestage arms: a guest that hangs resets, and
    // -no-reboot turns the reset into QEMU exiting, as for a panic.
    if arch.name == "x86_64" {
        command.args(["-device", "i6300esb"]);
        // Suspend to RAM, which q35 hides by default.
        command.args(["-global", "ICH9-LPC.disable_s3=0"]);
    }
    for accel in accelerators(arch)
        .into_iter()
        .filter(|a| !(no_hvf && *a == "hvf"))
    {
        command.args(["-accel", accel]);
    }
    command.args(arch.machine).args([
        "-m",
        edition.memory,
        "-smp",
        "2",
        "-no-reboot",
        // QEMU's user-mode network: DHCP and DNS from QEMU itself, and
        // the host's connection behind them. What NetworkManager meets on
        // a wired port.
        "-nic",
        "user,model=virtio-net-pci",
    ]);
    command
}

/// The image's kernel and initramfs, checked to exist.
fn ram_image(arch: Arch, dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let kernel = dir.join("vmlinuz");
    let initrd = dir.join("initramfs.cpio");
    if !kernel.is_file() || !initrd.is_file() {
        return Err(format!(
            "no image in {}; run `cargo xtask image --arch {}` first",
            dir.display(),
            arch.name
        ));
    }
    Ok((kernel, initrd))
}

/// The account on disks `install` makes, with itself as the password. A
/// development disk, for QEMU: the greeter needs someone to log in as, and
/// a test needs to know the password. An installed machine gets the
/// account its owner chooses.
const DEV_USER: &str = "hide";

/// How big `install` makes the disk. Sparse, so it costs what is written.
const DISK_SIZE: u64 = 16 << 30;
/// What `hide install` prints when it is done.
const INSTALL_MARKER: &str = "hide install: installed";
const INSTALL_FAILED: &str = "hide install: FAILED";

/// Makes `disk.raw` the way hideOS is installed on a machine: boots the
/// Minimal image from RAM with `hide install` as PID 1, the payload as one
/// virtio disk and the empty disk as the other. See ARCHITECTURE.md, "Disk
/// images are installed, not assembled": the builder's kernel cannot enable
/// fs-verity, a hideOS kernel can.
fn install(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let edition = find_edition(args)?;
    let disk = image_dir(edition, arch)?.join("disk.raw");
    install_disk(arch, edition, &disk)
}

/// Installs `edition` on a new disk image at `disk`.
fn install_disk(arch: Arch, edition: Edition, disk: &Path) -> Result<(), String> {
    install_disk_with(arch, edition, disk, "")
}

/// A new disk is a new machine: its firmware starts with the template's
/// variables, not with what the last test's boots left.
fn fresh_firmware_variables(dir: &Path) {
    for name in ["efivars.fd", "efivars-secure.fd"] {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// `install_disk`, with more arguments for `hide install`.
fn install_disk_with(arch: Arch, edition: Edition, disk: &Path, extra: &str) -> Result<(), String> {
    let dir = image_dir(edition, arch)?;
    install_payload(arch, edition, &dir, disk, extra)
}

/// `install_disk_with`, from the payload in `dir`.
fn install_payload(
    arch: Arch,
    edition: Edition,
    dir: &Path,
    disk: &Path,
    extra: &str,
) -> Result<(), String> {
    let user = format!("--user {DEV_USER} --password {DEV_USER} {extra}");
    install_payload_as(arch, edition, dir, disk, &user)
}

/// `install_payload` with every argument for `hide install` given: with
/// no `--user`, the disk is one first-boot setup has yet to set up.
fn install_payload_as(
    arch: Arch,
    edition: Edition,
    dir: &Path,
    disk: &Path,
    arguments: &str,
) -> Result<(), String> {
    let dir = dir.to_path_buf();
    // Every edition is installed by Minimal, as on a real machine.
    let (kernel, initrd) = ram_image(arch, &image_dir(MINIMAL, arch)?)?;
    let payload = dir.join("payload.tar");
    if !payload.is_file() {
        return Err(format!(
            "no payload in {}; run `cargo xtask image --arch {} --edition {}` first",
            dir.display(),
            arch.name,
            edition.name
        ));
    }
    let _ = fs::remove_file(disk);
    fs::File::create(disk)
        .and_then(|f| f.set_len(DISK_SIZE))
        .map_err(|e| format!("creating {}: {e}", disk.display()))?;
    fresh_firmware_variables(&image_dir(MINIMAL, arch)?);
    if edition.name != MINIMAL.name {
        fresh_firmware_variables(&dir);
    }

    let log = dir.join("install.log");
    let _ = fs::remove_file(&log);
    let mut command = qemu(arch, MINIMAL);
    command
        .arg("-kernel")
        .arg(&kernel)
        .arg("-initrd")
        .arg(&initrd)
        // Everything after `--` is the init's argv. Drives are numbered in
        // the order they are given: the disk is vda, the payload vdb.
        .arg("-append")
        .arg(format!(
            "console={} rdinit=/usr/bin/hide panic=-1 -- \
             install --payload /dev/vdb --disk /dev/vda --poweroff {arguments}",
            arch.console
        ))
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .arg("-drive")
        .arg(format!(
            "if=virtio,format=raw,readonly=on,file={}",
            payload.display()
        ));
    println!(
        "installing hideOS {} {} on {}",
        edition.name,
        arch.name,
        disk.display()
    );
    let serial = run_headless(
        command,
        &log,
        Duration::from_secs(900),
        &[INSTALL_MARKER, INSTALL_FAILED],
    )?;
    for line in serial.lines().filter(|l| l.contains("hide install:")) {
        println!("  {}", line.trim());
    }
    if serial.contains(INSTALL_MARKER) {
        Ok(())
    } else {
        Err(format!(
            "the install failed; serial log in {}",
            log.display()
        ))
    }
}

/// Runs QEMU with no display and the serial console in `log`, until the
/// guest exits, a line containing one of `stop_at` appears, or `timeout`.
/// Returns what the serial console printed.
fn run_headless(
    mut command: Command,
    log: &Path,
    timeout: Duration,
    stop_at: &[&str],
) -> Result<String, String> {
    command
        .args(["-display", "none", "-monitor", "none"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start QEMU: {e}"))?;
    let read = || {
        fs::read(log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    let outcome = loop {
        let serial = without_kernel_messages(&read());
        if stop_at.iter().any(|m| serial.contains(m)) {
            // Give a powering-off guest a moment to finish on its own.
            thread::sleep(Duration::from_secs(2));
            break Ok(());
        }
        if let Ok(Some(_)) = child.try_wait() {
            break Ok(());
        }
        if started.elapsed() > timeout {
            break Err(format!(
                "nothing after {}s; serial log in {}",
                timeout.as_secs(),
                log.display()
            ));
        }
        thread::sleep(Duration::from_millis(200));
    };
    let _ = child.kill();
    let _ = child.wait();
    outcome.map(|()| without_kernel_messages(&read()))
}

/// The serial console without the kernel's own messages. They are written
/// whenever the kernel has something to say, including into the middle of a
/// line a program is printing, which would hide that line from a search.
fn without_kernel_messages(serial: &str) -> String {
    let mut out = String::with_capacity(serial.len());
    let mut rest = serial;
    while let Some(start) = rest.find('[') {
        let (before, from) = rest.split_at(start);
        out.push_str(before);
        // "[   12.345678] ...\n"
        let stamp = from.find(']').and_then(|end| from.get(1..end)).filter(|t| {
            let t = t.trim_start();
            !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.') && t.contains('.')
        });
        match stamp {
            Some(_) => {
                rest = match from.find('\n') {
                    Some(newline) => from.get(newline + 1..).unwrap_or(""),
                    None => "",
                };
            }
            None => {
                out.push('[');
                rest = from.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

/// A writable copy of the firmware's variable store, and the two pflash
/// drives that give QEMU the firmware.
/// QEMU with UEFI firmware, to boot a disk. With `secure_boot`, the
/// firmware enforces signatures with the development key enrolled.
fn disk_qemu(
    arch: Arch,
    edition: Edition,
    dir: &Path,
    secure_boot: bool,
) -> Result<Command, String> {
    let secure = if secure_boot {
        Some(
            secure_firmware(arch)?
                .ok_or_else(|| format!("no Secure Boot firmware for {} yet", arch.name))?,
        )
    } else {
        None
    };
    let mut command = qemu_with(arch, edition, secure.is_some());
    uefi_firmware(arch, &mut command, dir, secure)?;
    Ok(command)
}

fn uefi_firmware(
    arch: Arch,
    command: &mut Command,
    dir: &Path,
    secure: Option<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    let secure_boot = secure.is_some();
    let (code, vars_template) = match secure {
        Some(firmware) => {
            // Secure Boot on x86 is enforced from SMM, where the variable
            // store is out of the OS's reach.
            command
                .args(["-machine", "smm=on"])
                .args(["-global", "driver=cfi.pflash01,property=secure,value=on"]);
            firmware
        }
        None => find_firmware(arch).ok_or_else(|| {
            format!(
                "no {} UEFI firmware found; `cargo xtask doctor` says where it was looked for",
                arch.name
            )
        })?,
    };
    // The machine's NVRAM: kept from boot to boot, as a real one keeps it —
    // hibernation and the boot loader leave variables for the next boot.
    // One per firmware, and a fresh one for each installed disk; see
    // `fresh_firmware_variables`.
    let vars = dir.join(if secure_boot {
        "efivars-secure.fd"
    } else {
        "efivars.fd"
    });
    if !vars.exists() {
        fs::copy(&vars_template, &vars).map_err(|e| format!("copying the variable store: {e}"))?;
    }
    command
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            code.display()
        ))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=1,file={}",
            vars.display()
        ));
    Ok(())
}

/// Firmware with Secure Boot on and the development key enrolled: Debian's
/// "snakeoil" OVMF, whose key — private half included, so that anyone can
/// sign for it — is the one `image` signs with. Copied out of the builder
/// once. It proves the mechanism, not ownership: see ARCHITECTURE.md,
/// "Security". `None` where there is none yet (aarch64).
fn secure_firmware(arch: Arch) -> Result<Option<(PathBuf, PathBuf)>, String> {
    if arch.name != "x86_64" {
        return Ok(None);
    }
    let root = workspace_root()?;
    let dir = root.join("target").join("firmware");
    let code = dir.join("OVMF_CODE_4M.secboot.fd");
    let vars = dir.join("OVMF_VARS_4M.snakeoil.fd");
    if !code.is_file() || !vars.is_file() {
        let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
        run(builder_command(&runtime, &root, false).args([
            "sh",
            "-c",
            "mkdir -p /src/target/firmware && cp /usr/share/OVMF/OVMF_CODE_4M.secboot.fd \
             /usr/share/OVMF/OVMF_VARS_4M.snakeoil.fd /src/target/firmware/",
        ]))?;
    }
    Ok(Some((code, vars)))
}

/// Where the development signing key lives in the work volume, and how it
/// is made: the snakeoil key, with the passphrase Debian publishes removed
/// so sbsign can use it unattended.
const DEV_KEYS: &str = "/work/keys/dev";
const DEV_KEYS_SCRIPT: &str = "test -f /work/keys/dev/db.key || { \
    mkdir -p /work/keys/dev && \
    openssl rsa -in /usr/share/ovmf/PkKek-1-snakeoil.key -passin pass:snakeoil \
        -out /work/keys/dev/db.key 2>/dev/null && \
    cp /usr/share/ovmf/PkKek-1-snakeoil.pem /work/keys/dev/db.crt; }";

fn boot(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let edition = find_edition(args)?;
    let test = args.iter().any(|a| a == "--test");
    let secure_boot = args.iter().any(|a| a == "--secure-boot");
    let from_disk = args.iter().any(|a| a == "--disk") || !edition.ram || secure_boot;
    let dir = image_dir(edition, arch)?;

    let mut command = if from_disk {
        disk_qemu(arch, edition, &dir, secure_boot)?
    } else {
        qemu(arch, edition)
    };
    if from_disk {
        // The real chain: firmware, systemd-boot, the UKI, hidestage
        // checking the seal, oxinit.
        let disk = dir.join("disk.raw");
        if !disk.is_file() {
            return Err(format!(
                "no disk in {}; run `cargo xtask install --arch {} --edition {}` first",
                dir.display(),
                arch.name,
                edition.name
            ));
        }
        command
            .arg("-drive")
            .arg(format!("if=virtio,format=raw,file={}", disk.display()));
        if !edition.ram && !test {
            // The desktop: a GPU without 3D, which Mesa drives with
            // llvmpipe, and a tablet, so the pointer follows the host's.
            command.args(DESKTOP_DEVICES).args(["-serial", "stdio"]);
            println!(
                "booting hideOS {} {} in a window; the serial console is here",
                edition.name, arch.name
            );
            return run(&mut command);
        }
    } else {
        let (kernel, initrd) = ram_image(arch, &dir)?;
        command
            .arg("-kernel")
            .arg(&kernel)
            .arg("-initrd")
            .arg(&initrd)
            // rdinit, not init: in an initramfs the kernel runs rdinit, and
            // the default /init does not exist in a hideOS root. panic=-1:
            // reboot at once on a panic, which -no-reboot turns into QEMU
            // exiting, so a failed boot ends instead of hanging.
            .arg("-append")
            .arg(format!(
                "console={} rdinit=/usr/bin/oxinit panic=-1",
                arch.console
            ));
    }

    if !test {
        println!(
            "booting hideOS {} {} (quit QEMU with Ctrl-A X)",
            edition.name, arch.name
        );
        command.arg("-nographic");
        return run(&mut command);
    }

    let log = dir.join("serial.log");
    let _ = fs::remove_file(&log);
    let from = if from_disk { "disk" } else { "RAM" };
    println!(
        "booting hideOS {} from {from} (timeout {}s)",
        arch.name,
        arch.timeout.as_secs()
    );
    let started = Instant::now();
    let serial = run_headless(command, &log, arch.timeout, &[BOOT_MARKER])?;
    match serial.lines().find(|l| l.contains(BOOT_MARKER)) {
        Some(line) => {
            println!(
                "{}: {} in {:.1}s",
                arch.name,
                line.trim(),
                started.elapsed().as_secs_f64()
            );
            Ok(())
        }
        None => Err(format!(
            "{}: no banner; serial log in {}",
            arch.name,
            log.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// seal-test

/// A guest whose serial console is this process's to read and type at.
struct Guest {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    output: std::sync::Arc<std::sync::Mutex<String>>,
    /// The software TPM this guest has, stopped with it.
    tpm: Option<Swtpm>,
}

/// A software TPM for one boot, its state in a directory, so that the next
/// boot with the same directory meets the same TPM: the same seeds, so the
/// same storage key. swtpm runs on the host — QEMU takes the TPM's data
/// channel as a descriptor over a Unix socket.
struct Swtpm {
    child: std::process::Child,
    socket: PathBuf,
}

impl Swtpm {
    fn start(state: &Path) -> Result<Swtpm, String> {
        fs::create_dir_all(state).map_err(|e| format!("creating {}: {e}", state.display()))?;
        // macOS limits a socket's path to 104 bytes; the image directory
        // may be longer, and may have spaces.
        let socket = env::temp_dir().join(format!("hideos-swtpm-{}.sock", std::process::id()));
        let _ = fs::remove_file(&socket);
        let child = Command::new("swtpm")
            .args(["socket", "--tpm2", "--flags", "startup-clear", "--tpmstate"])
            .arg(format!("dir={}", state.display()))
            .arg("--ctrl")
            .arg(format!("type=unixio,path={}", socket.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start swtpm (brew install swtpm): {e}"))?;
        let started = Instant::now();
        while !socket.exists() {
            if started.elapsed() > Duration::from_secs(10) {
                return Err("swtpm did not open its socket".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(Swtpm { child, socket })
    }

    fn attach(&self, command: &mut Command) {
        command
            .arg("-chardev")
            .arg(format!("socket,id=chrtpm,path={}", self.socket.display()))
            .args(["-tpmdev", "emulator,id=tpm0,chardev=chrtpm"])
            .args(["-device", "tpm-tis,tpmdev=tpm0"]);
    }
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
    }
}

impl Guest {
    /// The disk, through firmware that enforces Secure Boot.
    fn boot(arch: Arch, dir: &Path, disk: &Path) -> Result<Guest, String> {
        Guest::boot_with(arch, dir, disk, true, None)
    }

    /// The disk, with a software TPM whose state is in `tpm` when given.
    fn boot_tpm(
        arch: Arch,
        dir: &Path,
        disk: &Path,
        secure_boot: bool,
        tpm: Option<&Path>,
    ) -> Result<Guest, String> {
        let mut command = if tpm.is_some() && !secure_boot {
            let mut command = qemu_with(arch, MINIMAL, true);
            uefi_firmware(arch, &mut command, dir, None)?;
            command
        } else {
            disk_qemu(arch, MINIMAL, dir, secure_boot)?
        };
        command
            .arg("-drive")
            .arg(format!("if=virtio,format=raw,file={}", disk.display()));
        let swtpm = tpm.map(Swtpm::start).transpose()?;
        if let Some(swtpm) = &swtpm {
            swtpm.attach(&mut command);
        }
        let mut guest = Guest::spawn(arch, command)?;
        guest.tpm = swtpm;
        Ok(guest)
    }

    /// The disk, with or without Secure Boot, and optionally a second disk,
    /// read-only, as /dev/vdb.
    fn boot_with(
        arch: Arch,
        dir: &Path,
        disk: &Path,
        secure_boot: bool,
        second: Option<&Path>,
    ) -> Result<Guest, String> {
        let mut command = disk_qemu(arch, MINIMAL, dir, secure_boot)?;
        command
            .arg("-drive")
            .arg(format!("if=virtio,format=raw,file={}", disk.display()));
        if let Some(second) = second {
            command.arg("-drive").arg(format!(
                "if=virtio,format=raw,readonly=on,file={}",
                second.display()
            ));
        }
        Guest::spawn(arch, command)
    }

    /// Whether QEMU has exited: the guest powered off, or rebooted, which
    /// `-no-reboot` turns into an exit.
    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Minimal from RAM, with the disk attached as /dev/vda and nothing on
    /// it running: what an attacker with the disk in another machine has.
    fn boot_beside(arch: Arch, disk: &Path) -> Result<Guest, String> {
        let (kernel, initrd) = ram_image(arch, &image_dir(MINIMAL, arch)?)?;
        let mut command = qemu(arch, MINIMAL);
        command
            .arg("-kernel")
            .arg(kernel)
            .arg("-initrd")
            .arg(initrd)
            .arg("-append")
            .arg(format!(
                "console={} rdinit=/usr/bin/oxinit panic=-1",
                arch.console
            ))
            .arg("-drive")
            .arg(format!("if=virtio,format=raw,file={}", disk.display()));
        Guest::spawn(arch, command)
    }

    fn spawn(arch: Arch, mut command: Command) -> Result<Guest, String> {
        command
            .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", arch.qemu))?;
        let stdin = child.stdin.take().ok_or("QEMU's stdin")?;
        let mut stdout = child.stdout.take().ok_or("QEMU's stdout")?;
        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&output);
        thread::spawn(move || {
            use std::io::Read;
            let mut buffer = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                if let (Ok(mut text), Some(bytes)) = (sink.lock(), buffer.get(..n)) {
                    text.push_str(&String::from_utf8_lossy(bytes));
                }
            }
        });
        Ok(Guest {
            child,
            stdin,
            output,
            tpm: None,
        })
    }

    fn output(&self) -> String {
        self.output.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// Waits for `text` on the console, kernel messages aside.
    fn wait_for(&self, text: &str, timeout: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if without_kernel_messages(&self.output()).contains(text) {
                return true;
            }
            thread::sleep(Duration::from_millis(200));
        }
        false
    }

    /// Keys without Enter: for menus, where Enter chooses.
    fn type_keys(&mut self, keys: &str) -> Result<(), String> {
        use std::io::Write;
        self.stdin
            .write_all(keys.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("typing at the guest: {e}"))
    }

    fn type_line(&mut self, line: &str) -> Result<(), String> {
        use std::io::Write;
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("typing at the guest: {e}"))
    }

    /// Waits for the shell to read what is typed, which it does only once
    /// it has started; until then, typed lines may be lost.
    fn shell(&mut self) -> Result<(), String> {
        for _ in 0..10 {
            self.type_line("unsetopt zle; PS1=''")?;
            if self.run("print ready", Duration::from_secs(5)).is_ok() {
                return Ok(());
            }
        }
        Err(format!("the shell never answered:\n{}", self.output()))
    }

    /// Runs `command` at the shell and returns what it printed between two
    /// markers. The markers are built by the shell from pieces, so the
    /// echo of the typed line never matches them.
    fn run(&mut self, command: &str, timeout: Duration) -> Result<String, String> {
        let before = self.output().len();
        self.type_line(&format!(
            "a=@@; print \"${{a}}BEGIN\"; {{ {command} ; }} 2>&1; print \"${{a}}END\""
        ))?;
        if !self.wait_for_after(before, "@@END", timeout) {
            let output = self.output();
            let tail = output.get(output.len().saturating_sub(800)..).unwrap_or("");
            return Err(format!(
                "no answer to `{command}`; the console ends:\n{tail:?}"
            ));
        }
        let text = without_kernel_messages(self.output().get(before..).unwrap_or(""));
        let start = text
            .find("@@BEGIN")
            .map(|i| i + "@@BEGIN".len())
            .unwrap_or(0);
        let end = text.find("@@END").unwrap_or(text.len());
        Ok(text.get(start..end).unwrap_or("").trim().to_owned())
    }

    fn wait_for_after(&self, from: usize, text: &str, timeout: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if without_kernel_messages(self.output().get(from..).unwrap_or("")).contains(text) {
                return true;
            }
            thread::sleep(Duration::from_millis(200));
        }
        false
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The attacks H2's seal has to stop, on a disk of their own: see ROADMAP,
/// "H2 — The seal". Each one is something root on the running system can
/// do; the seal is what makes the next read, or the next boot, notice.
/// Networking on an installed Minimal: NetworkManager brings the wired
/// port up by itself, with DHCP and DNS, and the daemons log to oxlogd.
/// What real hardware needs that QEMU can show: a driver that is a module,
/// loaded by udev for a device it finds — an Intel e1000e network card,
/// which the virtio machine has no other driver for — the firmware the
/// drivers ask for, and CPU microcode at the front of the UKI's initrd.
fn hw_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    build_image(arch, MINIMAL, image_version()?, "", None)?;
    let disk = dir.join("hw-test.raw");
    fresh_firmware_variables(&dir);
    install_disk(arch, MINIMAL, &disk)?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);

    let mut command = disk_qemu(arch, MINIMAL, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .args(["-device", "e1000e,netdev=hw0", "-netdev", "user,id=hw0"]);
    let mut guest = Guest::spawn(arch, command)?;
    if !guest.wait_for("reached target default", arch.timeout) {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;
    let out = guest.run(
        "for i in {1..30}; do grep -q '^e1000e ' /proc/modules && break; sleep 1; done; \
         grep '^e1000e ' /proc/modules; for n in /sys/class/net/*; do \
         print \"${n##*/} $(basename $(readlink $n/device/driver) 2>/dev/null)\"; done",
        minute,
    )?;
    check(
        "udev loads the e1000e module for the card, and its interface appears",
        out.contains("e1000e ") && out.lines().any(|l| l.trim().ends_with(" e1000e")),
        &out,
    );
    let out = guest.run(
        "ls /usr/lib/firmware | wc -l; ls /usr/lib/firmware/iwlwifi-* | head -n 1; \
         uname -r; ls /usr/lib/modules/$(uname -r)/modules.dep",
        minute,
    )?;
    check(
        "the firmware and the modules of the running kernel are in the image",
        out.contains("iwlwifi-") && out.contains("modules.dep") && !out.contains("No such file"),
        &out,
    );
    let out = guest.run("modprobe -n -v btusb && print can-load", minute)?;
    check(
        "modprobe resolves a module's dependencies",
        out.contains("can-load"),
        &out,
    );
    guest.type_line("poweroff")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);
    let _ = fs::remove_file(&disk);

    // The UKI's initrd, read back in the builder: the microcode's cpio
    // comes first, where the kernel looks for it.
    if arch.name == "x86_64" {
        let uki = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "efi")
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("hideos-minimal-"))
            })
            .max_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
            .ok_or("no UKI in the image directory")?;
        let root = workspace_root()?;
        let relative = uki
            .strip_prefix(&root)
            .map_err(|_| "the UKI is outside the workspace".to_owned())?;
        let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
        let output = builder_command(&runtime, &root, false)
            .args(["sh", "-c"])
            .arg(
                "objcopy -O binary --only-section=.initrd \"/src/$1\" /tmp/initrd && \
                 cpio -it < /tmp/initrd 2>/dev/null | head -n 6",
            )
            .arg("sh")
            .arg(relative)
            .output()
            .map_err(|e| e.to_string())?;
        let listing = String::from_utf8_lossy(&output.stdout).into_owned();
        check(
            "the UKI's initrd starts with the CPU microcode",
            listing.contains("kernel/x86/microcode/GenuineIntel.bin")
                && listing.contains("kernel/x86/microcode/AuthenticAMD.bin"),
            &listing,
        );
    }

    if failures.is_empty() {
        println!("{}: what real hardware needs is there", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// The Workstation, installed and booted, and checked at its console:
/// what it adds to Minimal for applications and updates. The desktop
/// itself is `cargo xtask screenshot --edition workstation --login`.
/// Logs in at the greeter through QEMU's monitor, as a person types, and
/// waits for the session's display to appear. The greeter takes keys
/// before it is drawn, and loses them; so it is tried again, with the
/// field cleared first, until the session is there.
fn log_in(guest: &mut Guest, socket: &Path) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let mut monitor = UnixStream::connect(socket).map_err(|e| format!("QEMU monitor: {e}"))?;
    let mut send = |line: &str| -> Result<(), String> {
        monitor
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|e| format!("QEMU monitor: {e}"))
    };
    for _ in 0..5 {
        for _ in 0..12 {
            send("sendkey backspace")?;
            thread::sleep(Duration::from_millis(50));
        }
        for c in DEV_USER.chars() {
            send(&format!("sendkey {}", qcode(c)?))?;
            thread::sleep(Duration::from_millis(100));
        }
        send("sendkey ret")?;
        let out = guest.run(
            "for i in {1..60}; do ls /run/user/1000/wayland-[0-9] >/dev/null 2>&1 && break; \
             sleep 1; done; ls /run/user/1000/wayland-[0-9] >/dev/null 2>&1 && print session-up",
            Duration::from_secs(120),
        )?;
        if out.contains("session-up") {
            return Ok(());
        }
    }
    Err("the greeter never let the person in".to_owned())
}

/// A session on the Workstation desktop-test left, for looking at what a
/// test cannot say: one boot, logged in, and commands taken from files as
/// they appear, so that each question costs seconds rather than an install.
fn desktop_shell(args: &[String]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let arch = find_arch(args)?;
    let edition = EDITIONS
        .iter()
        .copied()
        .find(|e| e.name == "workstation")
        .ok_or("no workstation edition")?;
    let dir = image_dir(edition, arch)?;
    let disk = dir.join("desktop-test.raw");
    if !disk.exists() {
        return Err(format!(
            "no {}: run `cargo xtask desktop-test` first",
            disk.display()
        ));
    }
    let work = dir.join("desktop-shell");
    let (inbox, outbox) = (work.join("in"), work.join("out"));
    for d in [&inbox, &outbox] {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let socket = monitor_path("desktop-shell.sock");
    let _ = fs::remove_file(&socket);
    let mut command = disk_qemu(arch, edition, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .args(DESKTOP_DEVICES)
        .arg("-monitor")
        .arg(format!("unix:{},server,nowait", socket.display()));
    let mut guest = Guest::spawn(arch, command)?;
    if !guest.wait_for("reached target default", Duration::from_secs(300)) {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;
    log_in(&mut guest, &socket)?;
    let mut monitor = UnixStream::connect(&socket).map_err(|e| format!("QEMU monitor: {e}"))?;
    let mut send = |line: &str| -> Result<(), String> {
        monitor
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|e| format!("QEMU monitor: {e}"))
    };
    println!(
        "desktop-shell: logged in; commands go in {}",
        inbox.display()
    );
    loop {
        let mut names: Vec<PathBuf> = fs::read_dir(&inbox)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        names.sort();
        let Some(next) = names.first() else {
            thread::sleep(Duration::from_millis(500));
            continue;
        };
        let text = fs::read_to_string(next).map_err(|e| e.to_string())?;
        let name = next
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = fs::remove_file(next);
        match text.trim() {
            "quit" => break,
            "screenshot" => {
                let shot = monitor_path("desktop-shell.png");
                send(&format!("screendump {} -f png", shot.display()))?;
                thread::sleep(Duration::from_secs(3));
                let png = outbox.join(format!("{name}.png"));
                let _ = fs::rename(&shot, &png).or_else(|_| fs::copy(&shot, &png).map(|_| ()));
                println!("desktop-shell: {name}: {}", png.display());
            }
            command => {
                let out = guest.run(command, Duration::from_secs(600))?;
                fs::write(outbox.join(format!("{name}.txt")), &out).map_err(|e| e.to_string())?;
                println!("desktop-shell: {name} done");
            }
        }
    }
    Ok(())
}

fn desktop_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let edition = EDITIONS
        .iter()
        .copied()
        .find(|e| e.name == "workstation")
        .ok_or("no workstation edition")?;
    build_image(arch, edition, image_version()?, "", None)?;
    let dir = image_dir(edition, arch)?;
    let disk = dir.join("desktop-test.raw");
    fresh_firmware_variables(&dir);
    install_disk(arch, edition, &disk)?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);

    // A monitor besides the serial console: the greeter is typed at
    // through it, as a person would, and the screen saved from it.
    let socket = monitor_path("desktop-test.sock");
    let _ = fs::remove_file(&socket);
    let mut command = disk_qemu(arch, edition, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .args(DESKTOP_DEVICES)
        .arg("-monitor")
        .arg(format!("unix:{},server,nowait", socket.display()));
    let mut guest = Guest::spawn(arch, command)?;
    if !guest.wait_for(
        "reached target default",
        arch.timeout.max(Duration::from_secs(300)),
    ) {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;

    let out = guest.run(
        "for i in {1..30}; do grep -q serving /var/log/oxinit/hideupd.log && break; sleep 1; done; \
         dbus-send --system --print-reply --dest=os.hide.Update1 /os/hide/Update1 \
         os.hide.Update1.Deployments",
        minute,
    )?;
    check(
        "hideupd answers on the system bus, with the deployment that runs",
        out.contains("workstation") && out.contains("boolean true"),
        &out,
    );
    let out = guest.run(
        "pkaction --action-id os.hide.update.update --verbose",
        minute,
    )?;
    check(
        "polkit knows hideupd's actions",
        out.contains("Update the system"),
        &out,
    );
    let out = guest.run(
        "for i in {1..60}; do [[ $(nmcli -c no -t -f STATE general) == connected ]] && break; \
         sleep 1; done; flatpak remotes --system --columns=name; \
         flatpak remote-ls --system flathub --app --columns=application | head -n 3",
        Duration::from_secs(300),
    )?;
    check(
        "Flathub is there with no file in /etc, and its signed summary verifies",
        out.lines().any(|l| l.trim() == "flathub")
            && out.lines().filter(|l| l.trim().contains('.')).count() >= 3,
        &out,
    );
    // zsh's USERNAME, set by root, becomes that user: a sandbox as anyone.
    let out = guest.run(
        &format!(
            "( USERNAME={DEV_USER}; bwrap --ro-bind / / --dev /dev --proc /proc \
             --unshare-all --die-with-parent id -u ) && print sandboxed"
        ),
        minute,
    )?;
    check(
        "bubblewrap makes a sandbox for a user, unprivileged",
        out.contains("sandboxed"),
        &out,
    );
    let out = guest.run(
        "ls /usr/libexec/xdg-desktop-portal /usr/libexec/xdg-desktop-portal-cosmic \
         /usr/libexec/flatpak-system-helper; grep -c os.hide.Update1 /usr/bin/cosmic-settings",
        minute,
    )?;
    check(
        "the portals, Flatpak's helper, and Settings' Updates page are in the image",
        !out.contains("No such file") && out.lines().last().is_some_and(|l| l.trim() != "0"),
        &out,
    );

    // The desktop itself: logged in at the greeter, an application started
    // in the session — the terminal — kept up, and the screen saved while
    // it is.
    {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        log_in(&mut guest, &socket)?;
        let mut monitor = UnixStream::connect(&socket).map_err(|e| format!("QEMU monitor: {e}"))?;
        let mut send = |line: &str| -> Result<(), String> {
            monitor
                .write_all(format!("{line}\n").as_bytes())
                .map_err(|e| format!("QEMU monitor: {e}"))
        };
        let out = guest.run(
            &format!(
                "setopt nonomatch; \
                 for i in {{1..120}}; do ls /run/user/1000/wayland-[0-9] >/dev/null 2>&1 && break; \
                 sleep 1; done; sleep 20; \
                 for p in /proc/[0-9]*; do [[ $(<$p/comm) == cosmic-panel ]] && comp=${{p#/proc/}}; done 2>/dev/null; \
                 for p in /proc/[0-9]*; do [[ $(<$p/comm) == cosmic-term ]] && print \"already: ${{p#/proc/}} $(tr '\\0' ' ' < $p/cmdline)\"; done 2>/dev/null; \
                 ( USERNAME={DEV_USER}; \
                 xargs -0 sh -c 'exec env -i \"$@\" dbus-send --session --print-reply --dest=org.freedesktop.DBus / org.freedesktop.DBus.ListNames' _ < /proc/$comp/environ | grep -i term; \
                 {{ xargs -0 sh -c 'exec env -i \"$@\" RUST_BACKTRACE=1 cosmic-term --no-daemon' _ < /proc/$comp/environ; \
                 print \"term exited $?\"; }} > /tmp/term.log 2>&1 & ); \
                 sleep 60; grep -q 'term exited' /tmp/term.log || print term-alive; \
                 grep -v -e wgpu -e naga -e calloop -e sctk /tmp/term.log | tail -n 60"
            ),
            Duration::from_secs(300),
        )?;
        let shot = monitor_path("desktop-test.png");
        send(&format!("screendump {} -f png", shot.display()))?;
        thread::sleep(Duration::from_secs(3));
        let png = dir.join("desktop-test.png");
        let _ = fs::rename(&shot, &png).or_else(|_| fs::copy(&shot, &png).map(|_| ()));
        check(
            "an application started in the session stays up",
            out.contains("term-alive"),
            &out,
        );

        // `hide shell`, as the person, from a small image the test picks:
        // a container sharing the home, entered, a command run in it.
        let out = guest.run(
            &format!(
                "mkdir -p /etc/hide && print 'image = docker.io/library/alpine:latest' \
                 > /etc/hide/shell.conf; \
                 ( USERNAME={DEV_USER}; export HOME=/home/{DEV_USER} USER={DEV_USER} \
                 XDG_RUNTIME_DIR=/run/user/1000; cd; print hello > from-the-host; \
                 print 'cat /etc/os-release | head -n 1; cat ~/from-the-host; id -un; exit' \
                 | script -qc 'hide shell' /dev/null )"
            ),
            Duration::from_secs(600),
        )?;
        check(
            "hide shell enters a container that shares the home, as the same person",
            out.contains("Alpine") && out.contains("hello") && out.contains(DEV_USER),
            &out,
        );
        println!("        screen: {}", png.display());
        println!("        {}", out.replace('\n', "\n        "));
    }
    let _ = fs::remove_file(&socket);
    // With a session up — elogind, which leaves the cgroup oxinit gave it,
    // the person's own oxinit and its PipeWire — the machine still goes
    // down, and well inside oxinit's 90-second backstop.
    guest.type_line("poweroff")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    let off = guest.exited();
    check(
        "the Workstation powers off with a session up, within a minute",
        off,
        &guest
            .output()
            .lines()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n"),
    );
    drop(guest);
    // Kept when something failed, for `cargo xtask desktop-shell`.
    if failures.is_empty() {
        let _ = fs::remove_file(&disk);
    }

    if failures.is_empty() {
        println!("{}: the Workstation has what it adds", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// First-boot setup, as a Workstation from the installer meets it: an
/// encrypted disk opened by the setup key, no account, hidesetup on the
/// screen. Each page's call is made as the greeter's user, as hidesetup
/// makes it; then the login screen, the account, and a disk that asks for
/// the new passphrase.
fn setup_test(args: &[String]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let arch = find_arch(args)?;
    let edition = EDITIONS
        .iter()
        .copied()
        .find(|e| e.name == "workstation")
        .ok_or("no workstation edition")?;
    build_image(arch, edition, image_version()?, "", None)?;
    let dir = image_dir(edition, arch)?;
    let disk = dir.join("setup-test.raw");
    fresh_firmware_variables(&dir);
    // What the installer does for the Workstation: a random setup key on
    // the ESP; here a known one, to check it is gone after.
    install_payload_as(
        arch,
        edition,
        &dir,
        &disk,
        &format!("--encrypt {DEV_DISK_PASSPHRASE} --setup-key"),
    )?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let boot_timeout = arch.timeout.max(Duration::from_secs(300));
    let prompt = "Passphrase for the hideOS disk";
    let socket = monitor_path("setup-test.sock");
    let start = |socket: &Path| -> Result<Guest, String> {
        let _ = fs::remove_file(socket);
        let mut command = disk_qemu(arch, edition, &dir, false)?;
        command
            .arg("-drive")
            .arg(format!("if=virtio,format=raw,file={}", disk.display()))
            .args(DESKTOP_DEVICES)
            .arg("-monitor")
            .arg(format!("unix:{},server,nowait", socket.display()));
        Guest::spawn(arch, command)
    };
    let screen = |name: &str| -> Result<PathBuf, String> {
        let shot = monitor_path("setup-test.png");
        let mut monitor = UnixStream::connect(&socket).map_err(|e| format!("QEMU monitor: {e}"))?;
        monitor
            .write_all(format!("screendump {} -f png\n", shot.display()).as_bytes())
            .map_err(|e| format!("QEMU monitor: {e}"))?;
        thread::sleep(Duration::from_secs(3));
        let png = dir.join(name);
        let _ = fs::rename(&shot, &png).or_else(|_| fs::copy(&shot, &png).map(|_| ()));
        Ok(png)
    };
    // A call to os.hide.Setup1 as `user`: zsh's USERNAME, set by root,
    // becomes that user. SecureDisk makes two LUKS keyslots, each with
    // Argon2's memory-hard derivation: longer than dbus-send's 25 seconds
    // in a VM.
    let setup = |user: &str, call: &str| {
        format!(
            "( USERNAME={user}; dbus-send --system --print-reply --reply-timeout=300000 \
             --dest=os.hide.Update1 /os/hide/Setup1 os.hide.Setup1.{call} )"
        )
    };
    let greeter = |call: &str| setup("cosmic-greeter", call);

    println!("boot 1: the setup key opens the disk, and setup is on the screen");
    let mut guest = start(&socket)?;
    let up = guest.wait_for("reached target default", boot_timeout);
    check(
        "the setup key opens the disk, with nothing asked",
        up && !guest.output().contains(prompt),
        &guest.output(),
    );
    if !up {
        return Err("the disk did not boot".into());
    }
    guest.shell()?;
    let out = guest.run(
        "for i in {1..120}; do for p in /proc/[0-9]*; do [[ $(<$p/comm) == hidesetup ]] \
         && print \"setup up as $(stat -c %U $p)\" && break 2; done 2>/dev/null; sleep 1; done; \
         sleep 15; hide status",
        Duration::from_secs(300),
    )?;
    let png = screen("setup-test.png")?;
    check(
        "hidesetup is on the screen, as the greeter's user",
        out.contains("setup up as cosmic-greeter"),
        &out,
    );
    check(
        "hide status says the setup key opened it",
        out.contains("setup key"),
        &out,
    );
    println!("        screen: {}", png.display());

    let out = guest.run(
        &format!(
            "{}; {}; {}; {}; {}",
            greeter("Needed"),
            greeter("Languages"),
            greeter("Layouts") + " | grep -c '\"br\"'",
            greeter("Zones") + " | grep -c America/Sao_Paulo",
            greeter("DiskNeedsPassphrase"),
        ),
        minute,
    )?;
    check(
        "it offers languages, layouts and zones, and knows the disk needs a passphrase",
        out.contains("pt_BR.UTF-8")
            && out.contains("de_DE.UTF-8")
            && out.matches("boolean true").count() == 2
            && out.lines().filter(|l| l.trim() == "1").count() == 2,
        &out,
    );
    let out = guest.run(&setup("nobody", "SetLanguage string:pt_BR.UTF-8"), minute)?;
    check(
        "anyone but the greeter is refused",
        out.contains("AccessDenied"),
        &out,
    );
    let out = guest.run(&greeter("SetZone string:../../etc/shadow"), minute)?;
    check(
        "a zone outside zoneinfo is refused",
        out.contains("InvalidArgs"),
        &out,
    );

    let out = guest.run(
        &format!(
            "{} && {} && {} && {} && print chosen; cat /etc/environment; date +%z; \
             cat /var/lib/cosmic-greeter/.config/cosmic/com.system76.CosmicComp/v1/xkb_config",
            greeter("SetLanguage string:pt_BR.UTF-8"),
            greeter("SetKeyboard string:br string:"),
            greeter("SetZone string:America/Sao_Paulo"),
            greeter(&format!(
                "CreateAccount 'string:Ada Lovelace' string:ada string:{DEV_USER}"
            )),
        ),
        minute,
    )?;
    check(
        "the language, keyboard, time zone and account are taken",
        out.contains("chosen")
            && out.contains("LANG=pt_BR.UTF-8")
            && out.contains("-0300")
            && out.contains("layout: \"br\""),
        &out,
    );
    let out = guest.run(
        "id ada; stat -c '%U %a' /home/ada; \
         grep layout /home/ada/.config/cosmic/com.system76.CosmicComp/v1/xkb_config; \
         grep ^ada: /etc/subuid; \
         print clock $(</home/ada/.config/cosmic/com.system76.CosmicAppletTime/v1/military_time)",
        minute,
    )?;
    check(
        "the account administers the machine, with its home, keyboard, Brazil's 24-hour \
         clock and subordinate IDs",
        out.contains("uid=1000(ada)")
            && out.contains("wheel")
            && out.contains("ada 700")
            && out.contains("\"br\"")
            && out.contains("ada:")
            && out.contains("clock true"),
        &out,
    );

    let out = guest.run(&greeter(&format!("SecureDisk string:{DEV_USER}")), minute)?;
    let recovery = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("string \""))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or_default()
        .to_owned();
    check(
        "the disk takes the person's passphrase and gives a recovery key",
        recovery.len() >= 40 && recovery.contains('-'),
        &out,
    );
    let out = guest.run(
        &format!(
            "dev=$(cryptsetup status hideos-root | awk '/device:/ {{print $2}}'); \
             for key in {DEV_USER} {recovery} {DEV_DISK_PASSPHRASE}; do \
             print -rn -- $key | cryptsetup open --test-passphrase $dev --key-file - \
             && print \"opens: $key\" || print \"refused: $key\"; done; \
             mount -o ro /dev/vda1 /mnt && ls /mnt/EFI/hideos; umount /mnt"
        ),
        minute,
    )?;
    check(
        "the passphrase and the recovery key open it, the setup key no longer does and is gone",
        out.contains(&format!("opens: {DEV_USER}"))
            && out.contains(&format!("opens: {recovery}"))
            && out.contains(&format!("refused: {DEV_DISK_PASSPHRASE}"))
            && !out.contains("setup.key"),
        &out,
    );

    let out = guest.run(
        &format!(
            "{} && print finished; ls /var/lib/hide/setup-done; {}",
            greeter("Finish"),
            greeter("SetLanguage string:en_US.UTF-8")
        ),
        minute,
    )?;
    check(
        "setup finishes, and answers no one after",
        out.contains("finished") && !out.contains("No such file") && out.contains("AccessDenied"),
        &out,
    );

    // hidesetup leaves when Finish answers; here the calls were made
    // around it, so it is closed as it would close itself.
    guest.run(
        "for p in /proc/[0-9]*; do [[ $(<$p/comm) == hidesetup ]] && kill ${p#/proc/}; done \
         2>/dev/null; for i in {1..60}; do for p in /proc/[0-9]*; do \
         [[ $(<$p/comm) == cosmic-greeter ]] && break 2; done 2>/dev/null; sleep 1; done; sleep 10",
        Duration::from_secs(120),
    )?;
    let png = screen("setup-test-greeter.png")?;
    println!("        login screen: {}", png.display());
    log_in(&mut guest, &socket)?;
    let out = guest.run(
        "sleep 20; for p in /proc/[0-9]*; do [[ $(<$p/comm) == cosmic-panel ]] && \
         { stat -c %U $p; tr '\\0' '\\n' < $p/environ | grep ^LANG=; break; }; done 2>/dev/null",
        Duration::from_secs(120),
    )?;
    // The pointer away from the menu bar's corner, where it would hide the
    // logo.
    if let Ok(mut monitor) = UnixStream::connect(&socket) {
        let _ = monitor.write_all(b"mouse_move 640 400\n");
        thread::sleep(Duration::from_secs(1));
    }
    let png = screen("setup-test-desktop.png")?;
    check(
        "the login screen lets the new account in, in its language",
        out.contains("ada") && out.contains("LANG=pt_BR.UTF-8"),
        &out,
    );
    println!("        desktop: {}", png.display());
    guest.type_line("poweroff")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);

    println!("boot 2: the disk asks for the person's passphrase");
    let mut guest = start(&socket)?;
    let asked = guest.wait_for(prompt, boot_timeout);
    check("it asks", asked, &guest.output());
    guest.type_line(DEV_USER)?;
    let up = guest.wait_for("reached target default", boot_timeout);
    check("the person's passphrase opens it", up, &guest.output());
    drop(guest);
    let _ = fs::remove_file(&socket);

    if failures.is_empty() {
        let _ = fs::remove_file(&disk);
        println!("{}: first-boot setup sets the machine up", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

fn net_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    build_image(arch, MINIMAL, image_version()?, "", None)?;
    let disk = dir.join("net-test.raw");
    install_disk(arch, MINIMAL, &disk)?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);

    let mut guest = Guest::boot_with(arch, &dir, &disk, false, None)?;
    if !guest.wait_for("reached target default", arch.timeout) {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;
    let out = guest.run(
        "for i in {1..60}; do [[ $(nmcli -c no -t -f STATE general) == connected ]] && break; \
         sleep 1; done; nmcli -c no -t -f STATE general; nmcli -c no -t -f TYPE,STATE device",
        Duration::from_secs(90),
    )?;
    check(
        "NetworkManager connects the wired port by itself",
        out.lines().any(|l| l.trim() == "connected") && out.contains("ethernet:connected"),
        &out,
    );
    let out = guest.run("cat /etc/resolv.conf", minute)?;
    check(
        "it writes /etc/resolv.conf with the DHCP server's DNS",
        out.contains("nameserver 10.0.2.3"),
        &out,
    );
    let out = guest.run("getent ahosts github.com | head -1", minute)?;
    check(
        "names resolve",
        out.split_whitespace()
            .next()
            .is_some_and(|a| a.parse::<std::net::IpAddr>().is_ok()),
        &out,
    );
    let out = guest.run("iwctl device list >/dev/null; print iwctl=$?", minute)?;
    check("iwd answers on the bus", out.contains("iwctl=0"), &out);
    // The console up to here, before the log is printed on it.
    let console = without_kernel_messages(&guest.output());
    let out = guest.run("oxctl logs networkmanager | tail -3", minute)?;
    check(
        "NetworkManager logs to oxlogd, not the console",
        out.contains("<info>") && !console.contains("<info>"),
        &out,
    );
    drop(guest);
    let _ = fs::remove_file(&disk);

    if failures.is_empty() {
        println!("{}: the network comes up", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// Suspend and hibernation on an installed Minimal: the RTC wakes it from
/// suspend; after hibernation QEMU exits, starts again, and the same
/// session is back — with what it had in /tmp, which is memory only.
fn power_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    build_image(arch, MINIMAL, image_version()?, "", None)?;
    let disk = dir.join("power-test.raw");
    install_disk(arch, MINIMAL, &disk)?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);

    let mut guest = Guest::boot_with(arch, &dir, &disk, false, None)?;
    if !guest.wait_for("reached target default", arch.timeout) {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;
    let out = guest.run(
        "swapon --show=NAME --noheadings; print resume=$(cat /sys/power/resume)",
        minute,
    )?;
    check(
        "the swap file is on, and the kernel knows where to hibernate",
        out.contains("/swap/swapfile") && !out.contains("resume=0:0"),
        &out,
    );

    let out = guest.run(
        "rtcwake -m mem -s 5 >/dev/null; print rtcwake=$?; dmesg | grep -c 'PM: suspend exit'",
        minute,
    )?;
    check(
        "it suspends, and the clock wakes it",
        out.contains("rtcwake=0") && out.lines().any(|l| l.trim() == "1"),
        &out,
    );

    let marker = format!("hibernated-{}", std::process::id());
    guest.run(&format!("print {marker} > /tmp/marker; sync"), minute)?;
    // shutdown, not platform: QEMU's firmware has no S4 to enter.
    guest.type_line("print shutdown > /sys/power/disk; print disk > /sys/power/state")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < Duration::from_secs(300) {
        thread::sleep(Duration::from_millis(300));
    }
    check(
        "it hibernates and powers off",
        guest.exited(),
        &guest.output(),
    );
    drop(guest);

    let mut guest = Guest::boot_with(arch, &dir, &disk, false, None)?;
    let resumed = guest.wait_for("looking for a hibernated system", arch.timeout);
    // The session that hibernated: its shell, its /tmp. A fresh boot
    // would have neither, and would say `seal ok`.
    let out = if resumed {
        thread::sleep(Duration::from_secs(5));
        guest.run("cat /tmp/marker", minute).unwrap_or_default()
    } else {
        String::new()
    };
    check(
        "it resumes the same session, not a new boot",
        out.contains(&marker) && !guest.output().contains("seal ok"),
        &format!("{out}\n{}", guest.output()),
    );
    drop(guest);
    let _ = fs::remove_file(&disk);

    if failures.is_empty() {
        println!("{}: suspend and hibernation work", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// Lays out an installer medium in the builder: GPT, an ESP with the boot
/// manager, the installer's UKI and the recovery system the installer puts
/// on the disk it installs, and the payload, raw, in a partition
/// named hideos-payload — a tar archive is read as it is, and a FAT file
/// could not hold a payload over 4 GiB.
const INSTALLER_SCRIPT: &str = r#"set -eu
out=$1; uki=$2; manager=$3; payload=$4; boot=$5; recovery=$6
mib() { echo $(( ($(stat -c %s "$1") + 1048575) / 1048576 )); }
esp_mib=$(( $(mib "$uki") + $(mib "$manager") + $(mib "$recovery") + 64 ))
payload_mib=$(( $(mib "$payload") + 1 ))
rm -f "$out"
truncate -s $(( 1 + esp_mib + payload_mib + 1 ))M "$out"
sgdisk --clear     --new=1:1M:+${esp_mib}M --typecode=1:ef00 --change-name=1:hideos-installer     --new=2:0:+${payload_mib}M --typecode=2:8300 --change-name=2:hideos-payload     "$out" >/dev/null
esp=$(mktemp -u)
mkfs.vfat -C -F 32 -n HIDEOS "$esp" $(( esp_mib * 1024 )) >/dev/null
mmd -i "$esp" ::EFI ::EFI/BOOT ::EFI/Linux ::EFI/hideos ::loader
mcopy -i "$esp" "$manager" "::EFI/BOOT/$boot"
mcopy -i "$esp" "$uki" ::EFI/Linux/hideos-installer.efi
mcopy -i "$esp" "$recovery" ::EFI/hideos/recovery.efi
printf 'timeout 0
' > "$esp.conf"
mcopy -i "$esp" "$esp.conf" ::loader/loader.conf
dd if="$esp" of="$out" bs=1M seek=1 conv=notrunc status=none
dd if="$payload" of="$out" bs=1M seek=$(( 1 + esp_mib )) conv=notrunc status=none
rm -f "$esp" "$esp.conf"
"#;

/// Builds an installer medium for `--edition` (Minimal by default): the
/// installer is always Minimal's.
fn installer(args: &[String]) -> Result<PathBuf, String> {
    let arch = find_arch(args)?;
    let edition = find_edition(args)?;
    let version = image_version()?;
    build_image(arch, MINIMAL, version, "", None)?;
    if edition.name != MINIMAL.name {
        build_image(arch, edition, version, "", None)?;
    }
    let minimal = image_dir(MINIMAL, arch)?;
    let payload = image_dir(edition, arch)?.join("payload.tar");
    let out = workspace_root()?
        .join("target/images")
        .join(format!("installer-{}-{}.img", edition.name, arch.name));
    let in_builder = |path: &Path| -> Result<String, String> {
        let root = workspace_root()?;
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| format!("{} is outside the workspace", path.display()))?;
        Ok(format!("/src/{}", relative.display()))
    };
    let boot = if arch.name == "aarch64" {
        "BOOTAA64.EFI"
    } else {
        "BOOTX64.EFI"
    };
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    run(builder_command(&runtime, &workspace_root()?, false)
        .args(["sh", "-c", INSTALLER_SCRIPT, "installer"])
        .arg(in_builder(&out)?)
        .arg(in_builder(&minimal.join("installer.efi"))?)
        .arg(in_builder(&minimal.join("bootmanager.efi"))?)
        .arg(in_builder(&payload)?)
        .arg(boot)
        .arg(in_builder(&minimal.join("recovery.efi"))?))?;
    println!("  medium  {}", out.display());
    Ok(out)
}

/// The installer, driven as a person would: answers typed on its console.
/// Then the disk it made boots, opened with the recovery key it showed.
/// The Workstation's installer, which leaves the rest to first-boot
/// setup: it asks for the disk and whether to encrypt it, nothing more, and
/// the disk it writes opens with its setup key and starts hidesetup.
fn installer_setup_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let edition = find_edition(args)?;
    let medium = installer(args)?;
    let dir = image_dir(edition, arch)?;
    let disk = dir.join("installer-test.raw");
    let _ = fs::remove_file(&disk);
    fresh_firmware_variables(&dir);
    fs::File::create(&disk)
        .and_then(|f| f.set_len(DISK_SIZE))
        .map_err(|e| format!("creating {}: {e}", disk.display()))?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let boot_timeout = arch.timeout.max(Duration::from_secs(300));

    let mut command = disk_qemu(arch, MINIMAL, &image_dir(MINIMAL, arch)?, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .arg("-drive")
        .arg(format!(
            "if=virtio,format=raw,readonly=on,file={}",
            medium.display()
        ));
    let mut guest = Guest::spawn(arch, command)?;
    let mut asked = true;
    for (prompt, text) in [
        ("Install on which disk?", "1"),
        ("Type `erase` to continue", "erase"),
        ("Encrypt the disk?", "y"),
    ] {
        asked &= guest.wait_for(prompt, boot_timeout);
        guest.type_line(text)?;
        thread::sleep(Duration::from_millis(500));
    }
    check(
        "the installer asks for the disk and whether to encrypt it",
        asked,
        &guest.output(),
    );
    let installed = guest.wait_for("the first start sets it up", Duration::from_secs(1500));
    let output = guest.output();
    check(
        "it installs, asking for no passphrase and no account",
        installed && !output.contains("Disk passphrase:") && !output.contains("login name"),
        &output,
    );
    if guest.wait_for("Press Enter to turn the machine off.", minute) {
        guest.type_line("")?;
    }
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);

    let mut command = disk_qemu(arch, edition, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .args(DESKTOP_DEVICES);
    let mut guest = Guest::spawn(arch, command)?;
    let up = guest.wait_for("reached target default", boot_timeout);
    check(
        "the disk opens with its setup key, nothing asked",
        up && !guest.output().contains("Passphrase for the hideOS disk"),
        &guest.output(),
    );
    if up {
        guest.shell()?;
        let out = guest.run(
            "for i in {1..120}; do for p in /proc/[0-9]*; do [[ $(<$p/comm) == hidesetup ]] \
             && print setup-up && break 2; done 2>/dev/null; sleep 1; done; \
             hide status; ls /var/lib/hide/setup-done",
            Duration::from_secs(300),
        )?;
        check(
            "first-boot setup starts, on an encrypted disk with no account",
            out.contains("setup-up")
                && out.contains("encrypted (LUKS2)")
                && out.contains("setup key")
                && out.contains("No such file"),
            &out,
        );
    }
    drop(guest);

    if failures.is_empty() {
        let _ = fs::remove_file(&disk);
        println!(
            "{}: the {} installer leaves the rest to first-boot setup",
            arch.name, edition.name
        );
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

fn installer_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    if find_edition(args)?.name != MINIMAL.name {
        return installer_setup_test(args);
    }
    let medium = installer(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    let disk = dir.join("installer-test.raw");
    let _ = fs::remove_file(&disk);
    fresh_firmware_variables(&dir);
    fs::File::create(&disk)
        .and_then(|f| f.set_len(DISK_SIZE))
        .map_err(|e| format!("creating {}: {e}", disk.display()))?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let boot_timeout = arch.timeout.max(Duration::from_secs(300));

    // The empty disk first, so that it is the one the installer lists; the
    // firmware finds nothing to boot on it and boots the medium.
    let mut command = disk_qemu(arch, MINIMAL, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .arg("-drive")
        .arg(format!(
            "if=virtio,format=raw,readonly=on,file={}",
            medium.display()
        ));
    let mut guest = Guest::spawn(arch, command)?;
    let answer = |guest: &mut Guest, prompt: &str, text: &str| -> Result<bool, String> {
        if !guest.wait_for(prompt, boot_timeout) {
            return Ok(false);
        }
        guest.type_line(text)?;
        // Past the prompt before the next is looked for: two prompts can
        // share their beginning.
        thread::sleep(Duration::from_millis(500));
        Ok(true)
    };
    let mut asked = true;
    for (prompt, text) in [
        ("Install on which disk?", "1"),
        ("Type `erase` to continue", "erase"),
        ("Encrypt the disk?", "y"),
        ("Disk passphrase: ", DEV_DISK_PASSPHRASE),
        ("Disk passphrase, again: ", DEV_DISK_PASSPHRASE),
        ("Your login name: ", DEV_USER),
        ("Your password: ", DEV_USER),
        ("Your password, again: ", DEV_USER),
    ] {
        asked &= answer(&mut guest, prompt, text)?;
    }
    check(
        "the installer asks, and takes the answers",
        asked,
        &guest.output(),
    );
    let installed = guest.wait_for("hideOS is installed.", Duration::from_secs(1200));
    check("it installs", installed, &guest.output());
    let output = guest.output();
    let key = output
        .lines()
        .skip_while(|l| !l.contains("keep it away from this machine"))
        .map(str::trim)
        .find(|l| l.len() == 47 && l.matches('-').count() == 7)
        .unwrap_or_default()
        .to_owned();
    check("it shows a recovery key", !key.is_empty(), &output);
    answer(&mut guest, "Press Enter once it is written down.", "")?;
    answer(&mut guest, "Press Enter to turn the machine off.", "")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);

    // The disk alone: the recovery key, not the passphrase, opens it.
    let mut guest = Guest::boot_tpm(arch, &dir, &disk, false, None)?;
    let asked = guest.wait_for("Passphrase for the hideOS disk", boot_timeout);
    guest.type_line(&key)?;
    let up = asked && guest.wait_for("reached target default", boot_timeout);
    check(
        "the installed disk boots, opened with the recovery key",
        up,
        &guest.output(),
    );
    if up {
        guest.shell()?;
        // A file in the home, for the reinstall below to keep.
        let out = guest.run(
            &format!(
                "getent passwd {DEV_USER}; hide status; \
                 echo kept > /home/{DEV_USER}/kept; chown {DEV_USER}: /home/{DEV_USER}/kept"
            ),
            minute,
        )?;
        check(
            "with the account it was given",
            out.contains(&format!("{DEV_USER}:x:1000")) && out.contains("encrypted (LUKS2)"),
            &out,
        );
        guest.type_line("poweroff")?;
        let started = Instant::now();
        while !guest.exited() && started.elapsed() < minute {
            thread::sleep(Duration::from_millis(200));
        }
    }
    drop(guest);

    // The recovery system the installer put on the ESP, reached the way a
    // person reaches it: a key held as hideBoot starts, then its entry.
    let mut command = disk_qemu(arch, MINIMAL, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()));
    let mut guest = Guest::spawn(arch, command)?;
    let started = Instant::now();
    let mut menu = false;
    while started.elapsed() < boot_timeout {
        // Not a digit and not Enter: the menu ignores it.
        guest.type_keys("j")?;
        thread::sleep(Duration::from_millis(150));
        if guest.output().contains("(recovery)") {
            menu = true;
            break;
        }
    }
    check(
        "hideBoot's menu lists the recovery system",
        menu,
        &guest.output(),
    );
    if menu {
        thread::sleep(Duration::from_millis(500));
        guest.type_keys("2")?;
        let recovery = guest.wait_for("hideOS recovery", boot_timeout);
        guest.type_line("1")?;
        let listed = guest.wait_for("Which one should start next?", minute);
        guest.type_line("1")?;
        let chosen = guest.wait_for("starts next.", minute);
        check(
            "the recovery system starts, and chooses what starts next",
            recovery && listed && chosen,
            &guest.output(),
        );
        guest.type_line("4")?;
        let started = Instant::now();
        while !guest.exited() && started.elapsed() < minute {
            thread::sleep(Duration::from_millis(200));
        }
    }
    drop(guest);

    // The installer again, on the disk it installed: a reinstall that
    // keeps /home. The medium is booted first by its boot index, since the
    // disk now boots by itself; it still lists second, so the disk is 1.
    let mut command = disk_qemu(arch, MINIMAL, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!(
            "if=none,id=disk,format=raw,file={}",
            disk.display()
        ))
        .args(["-device", "virtio-blk-pci,drive=disk"])
        .arg("-drive")
        .arg(format!(
            "if=none,id=medium,format=raw,readonly=on,file={}",
            medium.display()
        ))
        .args(["-device", "virtio-blk-pci,drive=medium,bootindex=0"]);
    let mut guest = Guest::spawn(arch, command)?;
    let mut asked = true;
    for (prompt, text) in [
        ("Install on which disk?", "1"),
        ("Reinstall it, keeping /home?", "y"),
        ("Disk passphrase or recovery key: ", DEV_DISK_PASSPHRASE),
        ("Your login name: ", DEV_USER),
        ("Your password: ", DEV_USER),
        ("Your password, again: ", DEV_USER),
    ] {
        asked &= answer(&mut guest, prompt, text)?;
    }
    let reinstalled = asked && guest.wait_for("/home is as it was.", Duration::from_secs(1200));
    check(
        "the installer reinstalls over hideOS, opening the disk it finds",
        reinstalled,
        &guest.output(),
    );
    answer(&mut guest, "Press Enter to turn the machine off.", "")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);

    // QEMU's bootindex is passed to the firmware as a boot order it writes
    // into its variables, which leaves the disk out of the next boot: a
    // real machine's boot menu, choosing the stick, writes nothing. So the
    // variables start again, as a machine's would without the bootindex.
    fresh_firmware_variables(&dir);
    let mut guest = Guest::boot_tpm(arch, &dir, &disk, false, None)?;
    let asked = guest.wait_for("Passphrase for the hideOS disk", boot_timeout);
    guest.type_line(DEV_DISK_PASSPHRASE)?;
    let up = asked && guest.wait_for("reached target default", boot_timeout);
    let out = if up {
        guest.shell()?;
        guest.run(
            &format!("cat /home/{DEV_USER}/kept; stat -c %U /home/{DEV_USER}/kept"),
            minute,
        )?
    } else {
        guest.output()
    };
    check(
        "the reinstalled disk boots, and the home is still there, the account's",
        up && out.contains("kept") && out.contains(&format!("\n{DEV_USER}")),
        &out,
    );
    if up {
        guest.type_line("poweroff")?;
        let started = Instant::now();
        while !guest.exited() && started.elapsed() < minute {
            thread::sleep(Duration::from_millis(200));
        }
    }
    drop(guest);
    let _ = fs::remove_file(&disk);

    if failures.is_empty() {
        println!("{}: the installer installs", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// Builds `name` as a system extension for `image` (hex), signed or not,
/// into `output` (a directory under the workspace).
fn build_sysext(
    arch: Arch,
    name: &str,
    image: &str,
    signed: bool,
    output: &str,
) -> Result<(), String> {
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
        .args(["--arch", arch.name, "sysext", name, "--image", image])
        .args(["--output", output])
        .args(if signed {
            &["--sign", DEV_KEYS][..]
        } else {
            &[][..]
        }))
}

/// System extensions on an installed Minimal: one unsigned, refused when
/// added; one built for another system image, added but left out at boot;
/// one signed for this image, merged over /usr; and that one again with
/// its record's signature damaged, left out.
fn sysext_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    build_image(arch, MINIMAL, image_version()?, "", None)?;
    let image = fs::read_to_string(dir.join("image.digest")).map_err(|e| e.to_string())?;
    let image = image.trim().trim_start_matches("sha256:").to_owned();
    let out = format!("/src/target/images/minimal-{}/sysext", arch.name);
    let local = dir.join("sysext");
    let _ = fs::remove_dir_all(&local);
    for (sub, for_image, signed) in [
        ("right", image.as_str(), true),
        ("other", &"0".repeat(64)[..], true),
        ("unsigned", image.as_str(), false),
    ] {
        build_sysext(
            arch,
            "hello-sysext",
            for_image,
            signed,
            &format!("{out}/{sub}"),
        )?;
    }
    // The three, in one tar on a second disk; the guest unpacks it in /tmp.
    let bundle = dir.join("sysext-bundle.tar");
    run(Command::new("tar")
        .arg("-cf")
        .arg(&bundle)
        .arg("-C")
        .arg(&local)
        .args(["right", "other", "unsigned"]))?;

    let disk = dir.join("sysext-test.raw");
    install_disk(arch, MINIMAL, &disk)?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let archive = |sub: &str| format!("oci-archive:/tmp/x/{sub}/hello-sysext.sysext.oci.tar");

    let boot = |second: Option<&Path>| -> Result<Guest, String> {
        let mut guest = Guest::boot_with(arch, &dir, &disk, false, second)?;
        if !guest.wait_for("reached target default", arch.timeout) {
            return Err(format!("the disk did not boot:\n{}", guest.output()));
        }
        guest.shell()?;
        Ok(guest)
    };

    println!("boot 1: adding");
    let mut guest = boot(Some(&bundle))?;
    guest.run(
        "mkdir -p /tmp/x && tar -xf /dev/vdb -C /tmp/x && print unpacked",
        minute,
    )?;
    let out = guest.run(
        &format!("hide ext add {}; print exit=$?", archive("unsigned")),
        minute,
    )?;
    check(
        "an unsigned extension is refused",
        out.contains("carries no signature") && !out.contains("exit=0"),
        &out,
    );
    let out = guest.run(
        &format!("hide ext add {}; print exit=$?", archive("other")),
        minute,
    )?;
    check(
        "one built for another image is added",
        out.contains("exit=0"),
        &out,
    );
    reboot(guest)?;

    println!("boot 2: built for another image");
    let mut guest = boot(Some(&bundle))?;
    let console = guest.output();
    let out = guest.run("command -v hello-sysext; hide ext list", minute)?;
    check(
        "it is left out at boot",
        console.contains("left out: built for sha256:0000")
            && !out.contains("/usr/bin/hello-sysext"),
        &format!("{out}\n{console}"),
    );
    guest.run(
        "mkdir -p /tmp/x && tar -xf /dev/vdb -C /tmp/x && print unpacked",
        minute,
    )?;
    let out = guest.run(
        &format!("hide ext add {}; print exit=$?", archive("right")),
        minute,
    )?;
    check(
        "the one for this image is added",
        out.contains("exit=0"),
        &out,
    );
    reboot(guest)?;

    println!("boot 3: merged");
    let mut guest = boot(None)?;
    let out = guest.run(
        "hello-sysext; hide ext list; touch /usr/bin/x; print touch=$?",
        minute,
    )?;
    check(
        "it is merged over /usr, read-only",
        out.contains("hello from a system extension")
            && out.contains("merged")
            && out.contains("touch=1"),
        &out,
    );
    // The record's signature, one hex digit changed: what an attacker with
    // root could do to the store.
    let out = guest.run(
        "rec=/hideos/extensions/hello-sysext; sig=$(sed -n 's/^signature=//p' $rec); \
         [[ ${sig[1]} == 0 ]] && new=1 || new=0; \
         sed -i \"s/^signature=./signature=$new/\" $rec && print damaged",
        minute,
    )?;
    check("the record can be damaged", out.contains("damaged"), &out);
    reboot(guest)?;

    println!("boot 4: a damaged signature");
    let mut guest = boot(None)?;
    let console = guest.output();
    let out = guest.run("command -v hello-sysext; print checked", minute)?;
    check(
        "it is left out when the signature does not hold",
        console.contains("left out: hideOS did not sign it")
            && !out.contains("/usr/bin/hello-sysext"),
        &format!("{out}\n{console}"),
    );
    drop(guest);
    let _ = fs::remove_file(&disk);

    if failures.is_empty() {
        println!("{}: extensions merge only when they should", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// The development disk's passphrase: a disk image for QEMU.
const DEV_DISK_PASSPHRASE: &str = "hidedisk";

/// The encrypted root on an installed Minimal, with a software TPM.
fn crypt_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    build_image(arch, MINIMAL, image_version()?, "", None)?;
    let disk = dir.join("crypt-test.raw");
    install_disk_with(
        arch,
        MINIMAL,
        &disk,
        &format!("--encrypt {DEV_DISK_PASSPHRASE}"),
    )?;
    let tpm = dir.join("crypt-test.tpm");
    let _ = fs::remove_dir_all(&tpm);
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let prompt = "Passphrase for the hideOS disk";
    let boot_timeout = arch.timeout.max(Duration::from_secs(300));

    // btrfs's magic, which an unencrypted root has 64 KiB in: the first
    // 64 MiB after the ESP must not have it anywhere.
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = fs::File::open(&disk).map_err(|e| e.to_string())?;
        let mut region = vec![0u8; 64 << 20];
        file.seek(SeekFrom::Start(513 << 20))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut region).map_err(|e| e.to_string())?;
        let plain = region.windows(8).any(|w| w == b"_BHRfS_M");
        check(
            "the root is not readable on the disk",
            !plain,
            "btrfs's magic found in the clear",
        );
    }

    println!("boot 1: a passphrase, then the key sealed to the TPM");
    let mut guest = Guest::boot_tpm(arch, &dir, &disk, false, Some(&tpm))?;
    let asked = guest.wait_for(prompt, boot_timeout);
    check("it asks for the passphrase", asked, &guest.output());
    guest.type_line("wrong")?;
    let refused = guest.wait_for("that passphrase opens nothing", minute);
    check("a wrong passphrase opens nothing", refused, &guest.output());
    guest.type_line(DEV_DISK_PASSPHRASE)?;
    let up = guest.wait_for("reached target default", boot_timeout);
    check("the right one boots it", up, &guest.output());
    if !up {
        return Err("the encrypted disk did not boot".into());
    }
    guest.shell()?;
    let out = guest.run(
        "for i in {1..30}; do hide status 2>/dev/null | grep -q 'key sealed to the TPM' && break; sleep 1; done; hide status",
        minute,
    )?;
    check(
        "the first boot seals the key to the TPM",
        out.contains("opened by passphrase") && out.contains("key sealed to the TPM"),
        &out,
    );
    guest.type_line("poweroff")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < minute {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);

    println!("boot 2: the TPM opens it");
    let guest = Guest::boot_tpm(arch, &dir, &disk, false, Some(&tpm))?;
    let up = guest.wait_for("reached target default", boot_timeout);
    check(
        "it boots without asking",
        up && guest.output().contains("unlocked with the tpm") && !guest.output().contains(prompt),
        &guest.output(),
    );
    drop(guest);

    println!("boot 3: Secure Boot on, so PCR 7 changed: the TPM refuses");
    let mut guest = Guest::boot_tpm(arch, &dir, &disk, true, Some(&tpm))?;
    let asked = guest.wait_for(prompt, boot_timeout);
    check(
        "the TPM refuses a changed boot chain, and it asks",
        asked && guest.output().contains("the boot chain changed"),
        &guest.output(),
    );
    guest.type_line(DEV_DISK_PASSPHRASE)?;
    let up = guest.wait_for("reached target default", boot_timeout);
    check("the passphrase still opens it", up, &guest.output());
    drop(guest);

    println!("boot 4: no TPM");
    let guest = Guest::boot_tpm(arch, &dir, &disk, false, None)?;
    let asked = guest.wait_for(prompt, boot_timeout);
    check("without a TPM, it asks", asked, &guest.output());
    drop(guest);

    let _ = fs::remove_file(&disk);
    let _ = fs::remove_dir_all(&tpm);
    if failures.is_empty() {
        println!("{}: the encrypted root opens as it should", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

fn seal_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    let disk = dir.join("seal-test.raw");
    install_disk(arch, MINIMAL, &disk)?;
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    // Under Secure Boot the guest runs on emulated CPUs: see `qemu_with`.
    let boot_timeout = arch.timeout.max(Duration::from_secs(300));
    let minute = Duration::from_secs(60);

    println!("boot 1: attacks from the running system");
    let mut guest = Guest::boot(arch, &dir, &disk)?;
    let booted = guest.wait_for("reached target default", boot_timeout);
    check(
        "boots from the sealed disk",
        booted,
        "no `reached target default`",
    );
    if !booted {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;
    check(
        "the firmware enforced Secure Boot",
        guest.output().contains("hidestage: secure boot on"),
        "hidestage did not report `secure boot on`",
    );

    let out = guest.run("touch /usr/bin/intruder; print status=$?", minute)?;
    check(
        "writing to /usr fails",
        out.contains("Read-only file system") && out.contains("status=1"),
        &out,
    );

    let out = guest.run(
        "print hello > /etc/hide-seal-test && cat /etc/hide-seal-test",
        minute,
    )?;
    check("/etc stays writable", out.contains("hello"), &out);

    // The objects behind /usr/bin/btrfs, found by size with zsh's L
    // qualifier, each replaced by a copy with one byte changed — what
    // root, or a disk edited offline, can do to the store.
    let out = guest.run(
        "size=$(stat -c %s /usr/bin/btrfs); obj=(/hideos/objects/*/*(L$size)); \
         for o in $obj; do cp $o $o.new && print -n X | dd of=$o.new bs=1 seek=4096 \
         conv=notrunc status=none && mv -f $o.new $o; done; print tampered=${#obj}",
        minute,
    )?;
    check(
        "an object can be tampered with",
        out.contains("tampered=") && !out.contains("tampered=0"),
        &out,
    );
    let out = guest.run(
        "print 3 > /proc/sys/vm/drop_caches; cat /usr/bin/btrfs > /dev/null; print status=$?",
        minute,
    )?;
    check(
        "reading the tampered file fails with EIO",
        out.contains("Input/output error") && out.contains("status=1"),
        &out,
    );

    // The image's name pointed at another sealed file: what replacing the
    // system image looks like to the next boot. The booted image, by its
    // digest: the store holds others, the boot image an update checks
    // against among them.
    let out = guest.run(
        "for w in ${=$(</proc/cmdline)}; do [[ $w == hideos.image=sha256:* ]] && \
         img=/hideos/images/${w#hideos.image=sha256:}; done; \
         other=(/hideos/objects/*/*(.L+100000)); \
         ln -sfn ../objects/${other[1]#/hideos/objects/} $img && sync && print swapped",
        minute,
    )?;
    check("the image can be swapped", out.contains("swapped"), &out);
    guest.type_line("poweroff")?;
    let _ = guest.wait_for("reboot: Power down", minute);
    drop(guest);

    println!("boot 2: the swapped image");
    let guest = Guest::boot(arch, &dir, &disk)?;
    let refused = guest.wait_for(
        "does not match the one this kernel was signed with",
        boot_timeout,
    );
    check(
        "hidestage refuses the swapped image",
        refused && !guest.output().contains("reached target default"),
        &without_kernel_messages(&guest.output()),
    );
    drop(guest);

    // One byte of the UKI changed, from a system booted beside the disk:
    // what the seal cannot see, because it is the UKI that carries it. The
    // firmware has to refuse it before anything in it runs.
    println!("boot 3: the kernel image changed, from another system");
    let mut guest = Guest::boot_beside(arch, &disk)?;
    if !guest.wait_for("reached target default", boot_timeout) {
        return Err(format!(
            "Minimal did not boot from RAM:\n{}",
            guest.output()
        ));
    }
    guest.shell()?;
    let out = guest.run(
        "mount /dev/vda1 /mnt && uki=(/mnt/EFI/Linux/*.efi) && \
         size=$(stat -c %s $uki[1]) && print -n X | dd of=$uki[1] bs=1 \
         seek=$((size / 2)) conv=notrunc status=none && umount /mnt && print changed",
        minute,
    )?;
    check("the UKI can be changed", out.contains("changed"), &out);
    guest.type_line("poweroff")?;
    let _ = guest.wait_for("reboot: Power down", minute);
    drop(guest);

    println!("boot 4: the changed kernel image");
    let guest = Guest::boot(arch, &dir, &disk)?;
    // As the boot manager says it: systemd-boot passes on the firmware's
    // words, hideBoot names the status LoadImage returned.
    let started = Instant::now();
    let refused = loop {
        let output = guest.output();
        if ["Security Violation", "Access Denied", "ACCESS_DENIED"]
            .iter()
            .any(|w| output.contains(w))
        {
            break true;
        }
        if started.elapsed() > boot_timeout || output.contains("hidestage: starting") {
            break false;
        }
        thread::sleep(Duration::from_millis(500));
    };
    check(
        "the firmware refuses the changed UKI",
        refused && !guest.output().contains("hidestage: starting"),
        &guest.output(),
    );
    drop(guest);
    let _ = fs::remove_file(&disk);

    if failures.is_empty() {
        println!("{}: the seal holds", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

// ---------------------------------------------------------------------------
// update-test

/// Boots `disk` until the system comes up, through as many reboots as the
/// boot manager's attempts take: a failed boot ends QEMU (`-no-reboot`), and
/// the next start is the next attempt. Returns the guest at a shell.
fn boot_until_up(
    arch: Arch,
    dir: &Path,
    disk: &Path,
    second: Option<&Path>,
    log: &mut String,
) -> Result<Guest, String> {
    for attempt in 1..=6 {
        let mut guest = Guest::boot_with(arch, dir, disk, false, second)?;
        let started = Instant::now();
        loop {
            if guest.output().contains("reached target default") {
                guest.shell()?;
                return Ok(guest);
            }
            if guest.exited() || started.elapsed() > Duration::from_secs(180) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        log.push_str(&format!("attempt {attempt}:\n{}\n", guest.output()));
    }
    Err("the disk did not come up in six boots".to_owned())
}

/// The digest the guest is running, from its command line.
fn running_digest(guest: &mut Guest) -> Result<String, String> {
    let out = guest.run("cat /proc/cmdline", Duration::from_secs(30))?;
    out.split_whitespace()
        .find_map(|w| w.strip_prefix("hideos.image=sha256:"))
        .map(|d| d.chars().take(12).collect())
        .ok_or_else(|| format!("no hideos.image= in `{out}`"))
}

/// Restarts the guest: `reboot`, which oxinit turns into a reboot, which
/// `-no-reboot` turns into QEMU exiting.
fn reboot(mut guest: Guest) -> Result<(), String> {
    guest.type_line("reboot")?;
    let started = Instant::now();
    while !guest.exited() {
        if started.elapsed() > Duration::from_secs(60) {
            return Err("the guest did not reboot".to_owned());
        }
        thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

/// hideOS's development Secure Boot keys, in the builder's work volume:
/// PK, KEK and db, and the signed EFI signature lists the installer
/// enrolls — Microsoft's certificates beside hideOS's in KEK and db, from
/// tools/secureboot/microsoft. Made once; the lists are remade every time,
/// which is cheap. efitools is installed in a throwaway container: adding
/// it to the builder image would change the image's ID, which the
/// bootstrap's host-built stages hash, and rebuild everything.
const HIDEOS_KEYS: &str = "/work/keys/hideos-dev";
const SECUREBOOT_KEYS_SCRIPT: &str = r#"set -eu
out=/work/keys/hideos-dev
ms=/src/tools/secureboot/microsoft
mkdir -p "$out/enroll"
cd "$out"
command -v sign-efi-sig-list >/dev/null || {
    apt-get update -qq >/dev/null && apt-get install -y -qq efitools >/dev/null
}
[ -f owner ] || cat /proc/sys/kernel/random/uuid > owner
owner=$(cat owner)
for k in PK KEK db; do
    [ -f "$k.key" ] || openssl req -new -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes         -subj "/CN=hideOS development $k/" -keyout "$k.key" -out "$k.crt" 2>/dev/null
    cert-to-efi-sig-list -g "$owner" "$k.crt" "$k.esl"
done
microsoft=77fa9abd-0359-4d32-bd60-28f4e78f784b
list() {
    for c in "$@"; do
        openssl x509 -inform DER -in "$ms/$c.der" -out "/tmp/$c.pem"
        cert-to-efi-sig-list -g "$microsoft" "/tmp/$c.pem" "/tmp/$c.esl"
        cat "/tmp/$c.esl"
    done
}
cat KEK.esl > KEK-all.esl; list kek-2011 kek-2023 >> KEK-all.esl
cat db.esl > db-all.esl
list uefi-ca-2011 uefi-ca-2023 option-rom-ca-2023 windows-pca-2011 windows-uefi-ca-2023 >> db-all.esl
sign-efi-sig-list -g "$owner" -k PK.key -c PK.crt PK PK.esl enroll/PK.auth >/dev/null
sign-efi-sig-list -g "$owner" -k PK.key -c PK.crt KEK KEK-all.esl enroll/KEK.auth >/dev/null
sign-efi-sig-list -g "$owner" -k KEK.key -c KEK.crt db db-all.esl enroll/db.auth >/dev/null
ls -l enroll
"#;

fn secureboot_keys() -> Result<&'static str, String> {
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    run(builder_command(&runtime, &workspace_root()?, false).args([
        "sh",
        "-c",
        SECUREBOOT_KEYS_SCRIPT,
    ]))?;
    Ok(HIDEOS_KEYS)
}

/// H7's Secure Boot: a firmware with no keys at all, in setup mode, as a
/// machine is once its factory keys are cleared; hideOS installed, signed
/// with its own development keys; `hide secureboot enroll`; then the next
/// boot under those keys, enforced, and a changed UKI refused.
fn secureboot_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    if arch.name != "x86_64" {
        return Err("secureboot-test is x86_64 only, for now".to_owned());
    }
    let keys = secureboot_keys()?;
    let base = image_dir(MINIMAL, arch)?;
    build_image(arch, MINIMAL, image_version()?, "", None)?;
    build_image_signed(arch, MINIMAL, image_version()?, "secureboot", None, keys)?;
    let dir = base.join("secureboot");
    let disk = dir.join("secureboot-test.raw");
    install_payload(arch, MINIMAL, &dir, &disk, "")?;
    let (code, _) = secure_firmware(arch)?.ok_or("no Secure Boot firmware")?;
    // OVMF's variable store with no keys: setup mode.
    let empty = workspace_root()?.join("target/firmware/OVMF_VARS_4M.fd");
    if !empty.is_file() {
        let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
        run(builder_command(&runtime, &workspace_root()?, false).args([
            "sh",
            "-c",
            "mkdir -p /src/target/firmware && cp /usr/share/OVMF/OVMF_VARS_4M.fd /src/target/firmware/",
        ]))?;
    }
    fresh_firmware_variables(&dir);
    let boot = || -> Result<Guest, String> {
        let mut command = qemu_with(arch, MINIMAL, true);
        uefi_firmware(
            arch,
            &mut command,
            &dir,
            Some((code.clone(), empty.clone())),
        )?;
        command
            .arg("-drive")
            .arg(format!("if=virtio,format=raw,file={}", disk.display()));
        Guest::spawn(arch, command)
    };
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let timeout = Duration::from_secs(900);

    println!("boot 1: setup mode");
    let mut guest = boot()?;
    if !guest.wait_for("reached target default", timeout) {
        return Err(format!("the disk did not boot:\n{}", guest.output()));
    }
    guest.shell()?;
    let out = guest.run(
        "hide secureboot status; hide secureboot enroll; hide secureboot status",
        minute,
    )?;
    check(
        "a firmware in setup mode takes hideOS's keys",
        out.contains("in setup mode") && out.contains("PK enrolled") && !out.contains("hide: "),
        &out,
    );
    reboot(guest)?;

    println!("boot 2: under hideOS's keys");
    let mut guest = boot()?;
    let up = guest.wait_for("reached target default", timeout);
    let out = if up {
        guest.shell()?;
        guest.run("hide secureboot status", minute)?
    } else {
        guest.output()
    };
    check(
        "it boots under them, with Secure Boot on",
        up && out.contains("Secure Boot: on"),
        &out,
    );
    if up {
        // The UKI changed by a byte, past its signature: what an attacker
        // with the disk would do.
        let out = guest.run(
            "mkdir -p /run/esp && mount /dev/disk/by-partlabel/hideos-esp /run/esp && \
             for f in /run/esp/EFI/Linux/*.efi; do printf X | dd of=$f bs=1 seek=4096 \
             conv=notrunc status=none; done && umount /run/esp && print changed",
            minute,
        )?;
        check("the UKI can be changed", out.contains("changed"), &out);
        reboot(guest)?;
        println!("boot 3: the changed UKI");
        let guest = boot()?;
        let started = Instant::now();
        let refused = loop {
            let output = guest.output();
            if ["Security Violation", "Access Denied", "ACCESS_DENIED"]
                .iter()
                .any(|w| output.contains(w))
            {
                break true;
            }
            if started.elapsed() > timeout || output.contains("hidestage: starting") {
                break false;
            }
            thread::sleep(Duration::from_millis(500));
        };
        check(
            "the firmware refuses the changed UKI",
            refused,
            &guest.output(),
        );
    }
    let _ = fs::remove_file(&disk);
    if failures.is_empty() {
        println!("{}: Secure Boot is hideOS's", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// An update from a registry, the way a machine gets one: N+1 served as a
/// registry serves it, the channel pointed at it in /etc/hide/update.conf,
/// and `hide update` with nothing else. The registry is xtask's own, below:
/// read-only, plain HTTP, on the host, which QEMU's user network reaches at
/// 10.0.2.2.
fn registry_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    let version = image_version()?;
    println!("building N (version {version})");
    build_image(arch, MINIMAL, version, "", None)?;
    let short = |path: &Path| -> Result<String, String> {
        let digest = fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(digest
            .trim()
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect())
    };
    let n = short(&dir.join("image.digest"))?;
    println!("building N+1 (version {})", version + 1);
    build_image(arch, MINIMAL, version + 1, "next", None)?;
    let next_dir = dir.join("next");
    let n1 = short(&next_dir.join("image.digest"))?;
    println!("N is {n}, N+1 is {n1}");

    // N+1's OCI archive, unpacked: the layout the registry serves from.
    let layout = dir.join("registry");
    let _ = fs::remove_dir_all(&layout);
    fs::create_dir_all(&layout).map_err(|e| e.to_string())?;
    run(Command::new("tar")
        .arg("-xf")
        .arg(next_dir.join("image.oci.tar"))
        .arg("-C")
        .arg(&layout))?;
    let port = serve_registry(&layout)?;
    println!("registry on port {port}");

    let disk = dir.join("registry-test.raw");
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let mut log = String::new();
    fresh_firmware_variables(&dir);
    install_disk(arch, MINIMAL, &disk)?;
    let mut guest = boot_until_up(arch, &dir, &disk, None, &mut log)?;
    check("N boots", running_digest(&mut guest)? == n, &log);
    let out = guest.run(
        &format!(
            "mkdir -p /etc/hide && print 'registry = http://10.0.2.2:{port}/hideos\nchannel = edge' \
             > /etc/hide/update.conf && hide update"
        ),
        Duration::from_secs(600),
    )?;
    check(
        "plain `hide update` fetches the channel's image from the registry and stages it",
        out.contains("fetched http://10.0.2.2")
            && out.contains("minimal-edge")
            && out.contains("committed"),
        &out,
    );
    reboot(guest)?;
    let mut guest = boot_until_up(arch, &dir, &disk, None, &mut log)?;
    check(
        "N+1 boots after the update",
        running_digest(&mut guest)? == n1,
        &log,
    );
    guest.type_line("poweroff")?;
    let started = Instant::now();
    while !guest.exited() && started.elapsed() < Duration::from_secs(60) {
        thread::sleep(Duration::from_millis(200));
    }
    drop(guest);
    let _ = fs::remove_file(&disk);

    if failures.is_empty() {
        println!("{}: updates come from a registry", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// A read-only OCI distribution registry over plain HTTP, serving the image
/// in the OCI layout `layout` under any repository name and any tag: the
/// endpoints a pull uses — `/v2/`, a manifest by tag or digest, a blob by
/// digest — and nothing else. On a thread, for the life of xtask. Returns
/// the port, on every interface, so that QEMU's guests reach it.
fn serve_registry(layout: &Path) -> Result<u16, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let index = fs::read_to_string(layout.join("index.json")).map_err(|e| e.to_string())?;
    // The first manifest of the layout's index: hideforge's layouts have one.
    let digest = index
        .split("\"digest\"")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .ok_or("the layout's index names no manifest")?
        .to_owned();
    let listener = TcpListener::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let layout = layout.to_path_buf();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            // The headers, read and ignored.
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut parts = request.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
            let blob = |d: &str| {
                d.strip_prefix("sha256:")
                    .map(|hex| layout.join("blobs/sha256").join(hex))
            };
            let (status, kind, file, digest_header) = if path == "/v2/" || path == "/v2" {
                ("200 OK", "application/json", None, None)
            } else if let Some((_, reference)) = path.split_once("/manifests/") {
                let wanted = if reference.starts_with("sha256:") {
                    reference.to_owned()
                } else {
                    digest.clone()
                };
                match blob(&wanted).filter(|p| p.is_file()) {
                    Some(p) => (
                        "200 OK",
                        "application/vnd.oci.image.manifest.v1+json",
                        Some(p),
                        Some(wanted),
                    ),
                    None => ("404 Not Found", "text/plain", None, None),
                }
            } else if let Some((_, d)) = path.split_once("/blobs/") {
                match blob(d).filter(|p| p.is_file()) {
                    Some(p) => (
                        "200 OK",
                        "application/octet-stream",
                        Some(p),
                        Some(d.to_owned()),
                    ),
                    None => ("404 Not Found", "text/plain", None, None),
                }
            } else {
                ("404 Not Found", "text/plain", None, None)
            };
            let length = file
                .as_ref()
                .and_then(|p| fs::metadata(p).ok())
                .map(|m| m.len())
                .unwrap_or(if status.starts_with("200") { 2 } else { 0 });
            let mut out = &stream;
            let mut head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {length}\r\nConnection: close\r\n"
            );
            if let Some(d) = digest_header {
                head.push_str(&format!("Docker-Content-Digest: {d}\r\n"));
            }
            head.push_str("\r\n");
            let _ = out.write_all(head.as_bytes());
            if method != "HEAD" {
                match file {
                    Some(p) => {
                        if let Ok(mut f) = fs::File::open(p) {
                            let _ = std::io::copy(&mut f, &mut out);
                        }
                    }
                    None if status.starts_with("200") => {
                        let _ = out.write_all(b"{}");
                    }
                    None => {}
                }
            }
        }
    });
    Ok(port)
}

/// H3's "done when", on scratch disks. See ROADMAP, "H3".
fn update_test(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let dir = image_dir(MINIMAL, arch)?;
    // N, built here from this tree: an update runs the `hide` of the system
    // being updated, and an N left from an earlier build tests an earlier
    // `hide`.
    let version = image_version()?;
    println!("building N (version {version})");
    build_image(arch, MINIMAL, version, "", None)?;
    let n = fs::read_to_string(dir.join("image.digest")).map_err(|e| e.to_string())?;
    let n: String = n
        .trim()
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect();

    // N+1: the same system, one version later, which is a different image.
    // Its watchdog is shortened — hidestage's, and oxinit's boot deadline
    // and timeout after it — so that the hang below costs minutes, not a
    // quarter of an hour.
    println!("building N+1 (version {})", version + 1);
    build_image(
        arch,
        MINIMAL,
        version + 1,
        "next",
        Some("hideos.watchdog=45 oxinit.watchdog.boot-sec=45s oxinit.watchdog.timeout-sec=10s"),
    )?;
    let next_dir = dir.join("next");
    // The update as hideforge publishes it: an OCI image, here as an
    // oci-archive on a second disk.
    let next_image = next_dir.join("image.oci.tar");
    let n1 = fs::read_to_string(next_dir.join("image.digest")).map_err(|e| e.to_string())?;
    let n1: String = n1
        .trim()
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect();
    println!("N is {n} (version {version}), N+1 is {n1}");

    let disk = dir.join("update-test.raw");
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: &str| {
        println!("  {}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            println!("        {}", detail.replace('\n', "\n        "));
            failures.push(name.to_owned());
        }
    };
    let minute = Duration::from_secs(60);
    let mut log = String::new();

    // HIDEOS_UPDATE_TEST_STEPS=commit,prune: only those power cuts, for
    // when one is being chased.
    let only = env::var("HIDEOS_UPDATE_TEST_STEPS").ok();
    if only.is_none() {
        println!("a good update, then a rollback");
        install_disk(arch, MINIMAL, &disk)?;
        let mut guest = boot_until_up(arch, &dir, &disk, Some(&next_image), &mut log)?;
        check("N boots", running_digest(&mut guest)? == n, &log);
        // An object no image uses, as a power cut part-way through an
        // earlier update would leave: the update's collection removes it.
        // That it keeps N's objects, the rollback below shows: N mounts
        // with verity=require, and a missing object fails it.
        let orphan = format!("/hideos/objects/00/{}", "0".repeat(62));
        guest.run(
            &format!("mkdir -p /hideos/objects/00 && print orphan > {orphan}"),
            minute,
        )?;
        let out = guest.run(
            "hide update --image oci-archive:/dev/vdb",
            Duration::from_secs(300),
        )?;
        check("hide update stages N+1", out.contains("committed"), &out);
        // It went through hideupd, which ran it; and hideupd refuses
        // someone who is not root, with no polkit to ask. zsh's USERNAME,
        // set by root, becomes that user, as su would: Minimal has no su.
        let daemon = guest.run(
            &format!(
                "grep 'hide update' /var/log/oxinit/hideupd.log; \
                 ( USERNAME={DEV_USER}; hide rollback )"
            ),
            minute,
        )?;
        check(
            "the update ran in hideupd, which refuses anyone but root",
            daemon.contains("hide update --image") && daemon.contains("not authorized"),
            &daemon,
        );
        let gone = guest.run(&format!("[[ -e {orphan} ]] || print gone"), minute)?;
        check(
            "the update collects what no deployment uses",
            out.contains("collected") && gone.contains("gone"),
            &format!("{out}\n{gone}"),
        );
        reboot(guest)?;
        let mut guest = boot_until_up(arch, &dir, &disk, None, &mut log)?;
        check(
            "N+1 boots after the update",
            running_digest(&mut guest)? == n1,
            &log,
        );
        let status = guest.run("hide status", minute)?;
        check(
            "N+1 is marked good once it is up",
            status
                .lines()
                .any(|l| l.contains(&n1) && l.contains("good")),
            &status,
        );
        let out = guest.run("hide rollback", minute)?;
        check("hide rollback", out.contains("goes back"), &out);
        reboot(guest)?;
        let mut guest = boot_until_up(arch, &dir, &disk, None, &mut log)?;
        check(
            "N boots after the rollback",
            running_digest(&mut guest)? == n,
            &log,
        );
        drop(guest);

        println!("a bad update, which has to roll back by itself");
        install_disk(arch, MINIMAL, &disk)?;
        let mut guest = boot_until_up(arch, &dir, &disk, Some(&next_image), &mut log)?;
        let out = guest.run(
            "hide update --image oci-archive:/dev/vdb",
            Duration::from_secs(300),
        )?;
        check("hide update stages N+1", out.contains("committed"), &out);
        // N+1's image replaced by another sealed file: hidestage refuses it at
        // every attempt, as it would a corrupt or incomplete update.
        let out = guest.run(
            &format!(
                "other=(/hideos/objects/*/*(.L+100000)); \
                 ln -sfn ../objects/${{other[1]#/hideos/objects/}} /hideos/images/{n1}* && sync && print broken"
            ),
            minute,
        )?;
        check("N+1 can be broken", out.contains("broken"), &out);
        reboot(guest)?;
        let mut tries = String::new();
        let mut guest = boot_until_up(arch, &dir, &disk, None, &mut tries)?;
        let refusals = without_kernel_messages(&tries)
            .matches("does not match")
            .count();
        check(
            "N+1 is tried three times, then N boots",
            running_digest(&mut guest)? == n && refusals == 3,
            &format!("{refusals} refusals:\n{tries}"),
        );
        let status = guest.run("hide status", minute)?;
        check(
            "N+1 is marked bad, N good",
            status.lines().any(|l| l.contains(&n1) && l.contains("bad"))
                && status.lines().any(|l| l.contains(&n) && l.contains("good")),
            &status,
        );
        drop(guest);

        println!("an update that hangs, which the watchdog has to end");
        install_disk(arch, MINIMAL, &disk)?;
        let mut guest = boot_until_up(arch, &dir, &disk, Some(&next_image), &mut log)?;
        let out = guest.run(
            "hide update --image oci-archive:/dev/vdb",
            Duration::from_secs(300),
        )?;
        check("hide update stages N+1", out.contains("committed"), &out);
        // In /etc, which both share: boot-ok, the last unit, hangs on N+1
        // and only there. Nothing else stops a hung boot but the watchdog.
        let out = guest.run(
            &format!(
                "mkdir -p /etc/oxinit/units && cat > /etc/oxinit/units/boot-ok.toml <<'END'
[unit]
description = \"Mark this deployment good, or hang on N+1\"
requires = [\"multi-user\"]
after = [\"multi-user\"]

[service]
type = \"oneshot\"
exec = \"/usr/bin/sh -c 'grep -qx IMAGE_VERSION={} /usr/lib/os-release && sleep 100000; exec /usr/bin/hide boot-ok'\"
END
print hang-ready",
                version + 1
            ),
            minute,
        )?;
        check("N+1 can be made to hang", out.contains("hang-ready"), &out);
        reboot(guest)?;
        let mut hangs = String::new();
        let mut guest = boot_until_up(arch, &dir, &disk, None, &mut hangs)?;
        let resets = without_kernel_messages(&hangs)
            .matches("watchdog armed, 45 s")
            .count();
        check(
            "N+1 hangs three times, the watchdog resets it, then N boots",
            running_digest(&mut guest)? == n && resets == 3,
            &format!("{resets} resets:\n{hangs}"),
        );
        let status = guest.run("hide status", minute)?;
        check(
            "the hung N+1 is marked bad",
            status.lines().any(|l| l.contains(&n1) && l.contains("bad")),
            &status,
        );
        drop(guest);

        println!("an update whose kernel panics");
        install_disk(arch, MINIMAL, &disk)?;
        let mut guest = boot_until_up(arch, &dir, &disk, Some(&next_image), &mut log)?;
        let out = guest.run(
            "hide update --image oci-archive:/dev/vdb",
            Duration::from_secs(300),
        )?;
        check("hide update stages N+1", out.contains("committed"), &out);
        // The same override, crashing the kernel instead: panic=10 on the
        // command line turns the panic into a reboot, and a failed attempt.
        let out = guest.run(
            &format!(
                "mkdir -p /etc/oxinit/units && cat > /etc/oxinit/units/boot-ok.toml <<'END'
[unit]
description = \"Mark this deployment good, or panic on N+1\"
requires = [\"multi-user\"]
after = [\"multi-user\"]

[service]
type = \"oneshot\"
exec = \"/usr/bin/sh -c 'grep -qx IMAGE_VERSION={} /usr/lib/os-release && echo c > /proc/sysrq-trigger; exec /usr/bin/hide boot-ok'\"
END
print panic-ready",
                version + 1
            ),
            minute,
        )?;
        check(
            "N+1 can be made to panic",
            out.contains("panic-ready"),
            &out,
        );
        reboot(guest)?;
        let mut panics = String::new();
        let mut guest = boot_until_up(arch, &dir, &disk, None, &mut panics)?;
        let resets = panics.matches("Kernel panic").count();
        check(
            "N+1 panics three times, then N boots",
            running_digest(&mut guest)? == n && resets == 3,
            &format!("{resets} panics:\n{panics}"),
        );
        let status = guest.run("hide status", minute)?;
        check(
            "the panicking N+1 is marked bad",
            status.lines().any(|l| l.contains(&n1) && l.contains("bad")),
            &status,
        );
        drop(guest);
    }

    if only
        .as_deref()
        .is_none_or(|o| o.split(',').any(|x| x == "tamper"))
    {
        println!("an image changed after it was built");
        install_disk(arch, MINIMAL, &disk)?;
        let mut guest = boot_until_up(arch, &dir, &disk, Some(&next_image), &mut log)?;
        // One byte flipped 100 MiB in: inside a layer, whichever it is. In
        // /var/tmp, on the disk: the image no longer fits in /tmp, which is
        // memory.
        let out = guest.run(
            "dd if=/dev/vdb of=/var/tmp/image.tar bs=1M status=none && \
             printf x | dd of=/var/tmp/image.tar bs=1 seek=104857600 conv=notrunc status=none && \
             print tampered",
            minute,
        )?;
        check("N+1's image can be changed", out.contains("tampered"), &out);
        let out = guest.run(
            "hide update --image oci-archive:/var/tmp/image.tar; print exit=$?",
            Duration::from_secs(300),
        )?;
        let status = guest.run("hide status", minute)?;
        check(
            "hide update refuses it, and stages nothing",
            !out.contains("exit=0") && !out.contains("committed") && !status.contains(&n1),
            &format!("{out}\n{status}"),
        );
        drop(guest);
    }

    let steps = ["pull", "stage", "commit", "prune", "collect"];
    for step in steps.into_iter().filter(|s| {
        only.as_deref()
            .is_none_or(|o| o.split(',').any(|x| x == *s))
    }) {
        println!("a power cut after `{step}`");
        install_disk(arch, MINIMAL, &disk)?;
        let mut guest = boot_until_up(arch, &dir, &disk, Some(&next_image), &mut log)?;
        guest.type_line(&format!(
            "hide update --image oci-archive:/dev/vdb --crash-after {step}"
        ))?;
        let started = Instant::now();
        while !guest.exited() && started.elapsed() < Duration::from_secs(300) {
            thread::sleep(Duration::from_millis(300));
        }
        drop(guest);
        let mut after = String::new();
        let mut guest = boot_until_up(arch, &dir, &disk, None, &mut after)?;
        let running = running_digest(&mut guest)?;
        let status = guest.run("hide status", minute)?;
        // Before the commit the update never happened; after it, the new
        // system is what boots.
        let expected = if matches!(step, "commit" | "prune" | "collect") {
            &n1
        } else {
            &n
        };
        check(
            &format!(
                "after a cut at `{step}`, {} boots",
                if expected == &n { "N" } else { "N+1" }
            ),
            &running == expected,
            &format!("running {running}\n{status}\n{after}"),
        );
        drop(guest);
    }
    // Kept when chasing one step, to look at.
    if only.is_none() {
        let _ = fs::remove_file(&disk);
    }

    if failures.is_empty() {
        println!("{}: updates hold", arch.name);
        Ok(())
    } else {
        Err(format!("{}: {} check(s) failed", arch.name, failures.len()))
    }
}

/// What a desktop guest gets: virtio-gpu, keyboard and tablet.
// One display: QEMU's own VGA left beside the virtio GPU is a second output
// to the compositor, which opens windows on it, where no screenshot sees
// them. `-vga` given again replaces the default's.
const DESKTOP_DEVICES: &[&str] = &[
    "-vga",
    "none",
    "-device",
    "virtio-gpu-pci",
    "-device",
    "virtio-keyboard-pci",
    "-device",
    "virtio-tablet-pci",
];

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
    let edition = find_edition(args)?;
    if !edition.ram {
        let login = args.iter().any(|a| a == "--login");
        let then = flag(args, "--then")?.unwrap_or_default();
        return desktop_screenshot(arch, edition, login, then);
    }
    let dir = image_dir(edition, arch)?;
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
        let shot = monitor_path("screenshot.png");
        send(&format!("screendump {} -f png", shot.display()))?;
        thread::sleep(Duration::from_secs(2));
        fs::rename(&shot, &png)
            .or_else(|_| fs::copy(&shot, &png).map(|_| ()))
            .map_err(|e| format!("QEMU did not write the screenshot: {e}"))
    })();

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&socket);
    result?;
    println!("{}: {}", arch.name, png.display());
    Ok(())
}

/// Gives an installed disk's ESP what hideBoot's menu shows — an entry
/// being tried, a good one, one that failed, and the recovery system —
/// all names for the one image installed: what is shown is the menu, not
/// three systems.
const HIDEBOOT_ESP_SCRIPT: &str = r#"set -eu
disk=$1; recovery=$2; manager=${3:-}
esp="$disk@@1M"
if [ -n "$manager" ]; then
    mcopy -o -i "$esp" "$manager" ::/EFI/BOOT/BOOTX64.EFI
fi
name=$(mdir -b -i "$esp" ::/EFI/Linux | grep -i '\.efi$' | head -n 1)
name=${name##*/}
base=${name%.efi}; base=${base%%+*}
version=$(echo "$base" | awk -F- '{print $3}')
prefix=$(echo "$base" | awk -F- '{print $1 "-" $2}')
digest=$(echo "$base" | awk -F- '{print $4}')
tmp=$(mktemp)
mcopy -o -i "$esp" "::/EFI/Linux/$name" "$tmp"
mdel -i "$esp" "::/EFI/Linux/$name"
mcopy -o -i "$esp" "$tmp" "::/EFI/Linux/$prefix-$((version + 1))-$digest+2-1.efi"
mcopy -o -i "$esp" "$tmp" "::/EFI/Linux/$prefix-$version-$digest.efi"
mcopy -o -i "$esp" "$tmp" "::/EFI/Linux/$prefix-$((version - 1))-$digest+0-3.efi"
mmd -i "$esp" ::/EFI/Recovery 2>/dev/null || true
mcopy -o -i "$esp" "$recovery" ::/EFI/Recovery/hideos-recovery.efi
rm -f "$tmp"
mdir -b -i "$esp" ::/EFI/Linux ::/EFI/Recovery
"#;

/// `cargo xtask hideboot-screenshot`: hideBoot's menu, as the firmware
/// draws it, for youhide/hideBoot's README. A key held as it starts opens
/// the menu; the second picture has the recovery system chosen.
fn hideboot_screenshot(args: &[String]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let arch = find_arch(args)?;
    // --no-build: the image there is, while another command builds.
    if !args.iter().any(|a| a == "--no-build") {
        build_image(arch, MINIMAL, image_version()?, "", None)?;
    }
    let dir = image_dir(MINIMAL, arch)?;
    let disk = dir.join("hideboot-screenshot.raw");
    fresh_firmware_variables(&dir);
    install_disk(arch, MINIMAL, &disk)?;
    let in_builder = |path: &Path| -> Result<String, String> {
        let root = workspace_root()?;
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| format!("{} is outside the workspace", path.display()))?;
        Ok(format!("/src/{}", relative.display()))
    };
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    // --manager FILE: a hideBoot built from a checkout, unsigned — this
    // boot has no Secure Boot — for pictures of a change not yet in a
    // recipe.
    let manager = match args.iter().position(|a| a == "--manager") {
        Some(i) => {
            let path = args.get(i + 1).ok_or("--manager takes a file")?;
            let path = fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
            Some(in_builder(&path)?)
        }
        None => None,
    };
    run(builder_command(&runtime, &workspace_root()?, false)
        .args(["sh", "-c", HIDEBOOT_ESP_SCRIPT, "hideboot-esp"])
        .arg(in_builder(&disk)?)
        .arg(in_builder(&dir.join("recovery.efi"))?)
        .args(manager))?;

    // --display WxH: the screen QEMU's display announces through EDID,
    // the modes the firmware then offers — 1920x1080, 2560x1440, 3840x2160.
    let display: Vec<String> = match flag(args, "--display")? {
        Some(size) => {
            let (w, h) = size.split_once('x').ok_or("--display takes WIDTHxHEIGHT")?;
            vec![
                "-vga".into(),
                "none".into(),
                "-device".into(),
                format!("VGA,edid=on,xres={w},yres={h},vgamem_mb=64"),
            ]
        }
        None => vec!["-vga".into(), "std".into()],
    };
    let socket = dir.join("monitor.sock");
    let _ = fs::remove_file(&socket);
    let log = dir.join("hideboot-serial.log");
    let mut command = disk_qemu(arch, MINIMAL, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .args(&display)
        .args(["-display", "none"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .arg("-monitor")
        .arg(format!("unix:{},server,nowait", socket.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", arch.qemu))?;
    let serial = || fs::read_to_string(&log).unwrap_or_default();
    let shots = [
        ("hideboot-menu.png", 0usize),
        ("hideboot-menu-recovery.png", 3),
    ];
    let result = (|| -> Result<Vec<PathBuf>, String> {
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
        let mut send = |line: &str| -> Result<(), String> {
            monitor
                .write_all(format!("{line}\n").as_bytes())
                .map_err(|e| format!("QEMU monitor: {e}"))
        };
        // A key down as hideBoot starts: one every 150 ms until the menu
        // is up. Not a digit and not Enter, which the menu would act on.
        while !serial().contains("(recovery)") {
            if started.elapsed() > Duration::from_secs(120) {
                return Err(format!(
                    "hideBoot's menu did not open; see {}",
                    log.display()
                ));
            }
            send("sendkey j")?;
            thread::sleep(Duration::from_millis(150));
        }
        thread::sleep(Duration::from_secs(1));
        let mut saved = Vec::new();
        let mut moved = 0;
        for (name, down) in shots {
            while moved < down {
                send("sendkey down")?;
                moved += 1;
                thread::sleep(Duration::from_millis(400));
            }
            thread::sleep(Duration::from_secs(1));
            let shot = monitor_path(name);
            send(&format!("screendump {} -f png", shot.display()))?;
            thread::sleep(Duration::from_secs(2));
            let png = dir.join(name);
            fs::rename(&shot, &png)
                .or_else(|_| fs::copy(&shot, &png).map(|_| ()))
                .map_err(|e| format!("QEMU did not write {name}: {e}"))?;
            saved.push(png);
        }
        Ok(saved)
    })();
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_file(&disk);
    for png in result? {
        println!("{}: {}", arch.name, png.display());
    }
    Ok(())
}

/// A path QEMU's monitor can be given: its commands are split on spaces, and
/// the checkout's path may have them.
fn monitor_path(name: &str) -> PathBuf {
    env::temp_dir().join(format!("hideos-{}-{name}", std::process::id()))
}

/// How long the desktop gets to reach the greeter before the picture.
/// llvmpipe on two CPUs is slow to draw the first frame.
const DESKTOP_WAIT: Duration = Duration::from_secs(90);

/// Boots a desktop edition from its disk, waits for the greeter, and saves
/// the screen.
/// `--then`, after logging in: steps separated by `;` — `key:QCODES` presses
/// keys together (`meta_l-t`), `type:TEXT` types a line and Enter,
/// `wait:SECONDS` waits.
fn desktop_screenshot(arch: Arch, edition: Edition, login: bool, then: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let dir = image_dir(edition, arch)?;
    let disk = dir.join("disk.raw");
    if !disk.is_file() {
        return Err(format!(
            "no disk in {}; run `cargo xtask install --edition {}` first",
            dir.display(),
            edition.name
        ));
    }
    let socket = dir.join("monitor.sock");
    let _ = fs::remove_file(&socket);
    let png = dir.join("screenshot.png");
    let _ = fs::remove_file(&png);
    let log = dir.join("serial.log");

    let mut command = disk_qemu(arch, edition, &dir, false)?;
    command
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()))
        .args(DESKTOP_DEVICES)
        .args(["-display", "none"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .arg("-monitor")
        .arg(format!("unix:{},server,nowait", socket.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", arch.qemu))?;
    println!(
        "booting hideOS {} {}; picture in {}s",
        edition.name,
        arch.name,
        DESKTOP_WAIT.as_secs()
    );
    thread::sleep(DESKTOP_WAIT);
    let shot = monitor_path("screenshot.png");
    let result = UnixStream::connect(&socket)
        .and_then(|mut monitor| {
            if login {
                // The greeter starts with the password field focused.
                println!(
                    "logging in as {DEV_USER}; picture in {}s",
                    DESKTOP_WAIT.as_secs()
                );
                for c in DEV_USER.chars() {
                    let key = qcode(c).map_err(std::io::Error::other)?;
                    monitor.write_all(format!("sendkey {key}\n").as_bytes())?;
                    thread::sleep(Duration::from_millis(100));
                }
                monitor.write_all(b"sendkey ret\n")?;
                thread::sleep(DESKTOP_WAIT);
            }
            for step in then.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                match step.split_once(':') {
                    Some(("key", keys)) => {
                        monitor.write_all(format!("sendkey {keys}\n").as_bytes())?;
                        thread::sleep(Duration::from_secs(2));
                    }
                    Some(("type", text)) => {
                        for c in text.chars() {
                            let key = qcode(c).map_err(std::io::Error::other)?;
                            monitor.write_all(format!("sendkey {key}\n").as_bytes())?;
                            thread::sleep(Duration::from_millis(60));
                        }
                        monitor.write_all(b"sendkey ret\n")?;
                        thread::sleep(Duration::from_secs(2));
                    }
                    // The pointer, to `X Y` on the tablet: away from what it
                    // would hide.
                    Some(("mouse", at)) => {
                        monitor.write_all(format!("mouse_move {at}\n").as_bytes())?;
                        thread::sleep(Duration::from_secs(1));
                    }
                    Some(("wait", seconds)) => {
                        let seconds = seconds.parse().unwrap_or(5);
                        thread::sleep(Duration::from_secs(seconds));
                    }
                    _ => return Err(std::io::Error::other(format!("unknown step `{step}`"))),
                }
            }
            monitor.write_all(format!("screendump {} -f png\n", shot.display()).as_bytes())
        })
        .map_err(|e| format!("QEMU monitor: {e}"));
    thread::sleep(Duration::from_secs(3));
    let _ = fs::rename(&shot, &png).or_else(|_| fs::copy(&shot, &png).map(|_| ()));
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&socket);
    result?;
    if !png.is_file() {
        return Err(format!(
            "QEMU did not write the screenshot; serial log in {}",
            log.display()
        ));
    }
    println!("{}: {}", arch.name, png.display());
    Ok(())
}

/// `cargo xtask publish`: the image `cargo xtask image` last built for an
/// edition, pushed to the channel's tag on GitHub's registry, by hand. The
/// token is gh's — `gh auth refresh -s write:packages` once — written for
/// skopeo into a file only the builder reads, and removed after.
fn publish(args: &[String]) -> Result<(), String> {
    let arch = find_arch(args)?;
    let edition = find_edition(args)?;
    let channel = flag(args, "--channel")?.ok_or("--channel stable|beta|edge is required")?;
    if !["stable", "beta", "edge"].contains(&channel) {
        return Err(format!(
            "`{channel}` is not a channel: stable, beta or edge"
        ));
    }
    if arch.name != "x86_64" {
        return Err("only x86_64 is published for now".to_owned());
    }
    let dir = image_dir(edition, arch)?;
    let archive = dir.join("image.oci.tar");
    if !archive.is_file() {
        return Err(format!(
            "no image in {}; run `cargo xtask image --edition {}`",
            dir.display(),
            edition.name
        ));
    }
    let token = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|e| format!("running gh: {e}"))?;
    if !token.status.success() {
        return Err("gh has no token: `gh auth login`".to_owned());
    }
    let token = String::from_utf8_lossy(&token.stdout).trim().to_owned();
    let root = workspace_root()?;
    let auth = root.join("target/.ghcr-auth.json");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let encoded = base64_encode(format!("youhide:{token}").as_bytes());
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&auth)
            .map_err(|e| e.to_string())?;
        std::io::Write::write_all(
            &mut file,
            format!("{{\"auths\":{{\"ghcr.io\":{{\"auth\":\"{encoded}\"}}}}}}").as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    let target = format!("docker://ghcr.io/youhide/hideos:{}-{channel}", edition.name);
    let relative = archive
        .strip_prefix(&root)
        .map_err(|_| "the image is outside the workspace".to_owned())?;
    let runtime = container_runtime().ok_or("neither docker nor podman is on PATH")?;
    let result = run(builder_command(&runtime, &root, false)
        .args([
            "skopeo",
            "copy",
            "--authfile",
            "/src/target/.ghcr-auth.json",
        ])
        .arg(format!("oci-archive:/src/{}", relative.display()))
        .arg(&target));
    let _ = fs::remove_file(&auth);
    result?;
    println!("published {target}");
    Ok(())
}

/// Standard base64, for the registry auth file: the one place xtask needs it.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let index = ((n >> (18 - 6 * i)) & 63) as usize;
                out.push(char::from(TABLE[index]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Publishes `site/` and the latest x86_64 screenshot to the `gh-pages`
/// branch, which GitHub Pages serves. By hand, on purpose: nothing deploys
/// on push. The branch holds only the built site, one commit per publish,
/// each naming the main commit it came from.
fn publish_site() -> Result<(), String> {
    let root = workspace_root()?;
    // The wiki, rendered now from the pages as committed: site/wiki is not
    // checked in, and each page names the commit it was built from.
    let pages = wiki::generate(&root)?;
    println!("wiki: {pages} pages");
    // Everything in site/, and the console's picture from docs/images: the
    // pictures that were looked at and committed, not whatever the last
    // local run left in target/.
    let script = r#"
set -eu
root="$1"
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
git -C "$tree" rm -rqf --ignore-unmatch . >/dev/null
cp -R "$root/site/." "$tree/"
cp "$root/docs/images/minimal-console.png" "$tree/screenshot.png"
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
        .arg(&root))
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
        '&' => shifted("7"),
        '!' => shifted("1"),
        '@' => shifted("2"),
        '#' => shifted("3"),
        '%' => shifted("5"),
        '(' => shifted("9"),
        ')' => shifted("0"),
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

#[cfg(test)]
mod tests {
    #[test]
    fn base64_as_registries_read_it() {
        assert_eq!(super::base64_encode(b"youhide:abc"), "eW91aGlkZTphYmM=");
        assert_eq!(super::base64_encode(b"ab"), "YWI=");
        assert_eq!(super::base64_encode(b"abc"), "YWJj");
        assert_eq!(super::base64_encode(b""), "");
    }

    use super::without_kernel_messages;

    #[test]
    fn kernel_messages_come_out_even_mid_line() {
        let serial = "[    1.405627] Run /usr/bin/oxinit as init process\n\
                      hideOS: booted o[    1.422250] echo (76) used greatest stack depth\n\
                      n hideos\n\
                      a [bracket] stays\n";
        assert_eq!(
            without_kernel_messages(serial),
            "hideOS: booted on hideos\na [bracket] stays\n"
        );
    }
}
