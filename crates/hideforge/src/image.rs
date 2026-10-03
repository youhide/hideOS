//! Assembling an image from stage-2 outputs.
//!
//! For now an image is an initramfs: the whole root in one cpio archive, with
//! the kernel next to it. H2 replaces this with a composefs image on a disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use hideforge_recipe::{InputHash, RecipeSet, Stage};

use crate::layout::Layout;
use crate::output;

/// Merges the run closure of `name` into a scratch root, archives it as
/// `output/initramfs.cpio`, and copies `kernel`'s image to `output/vmlinuz`.
/// What `assemble` writes, besides the image root.
pub struct Outputs<'a> {
    pub dir: &'a Path,
    /// The kernel recipe, whose vmlinuz is copied next to the image.
    pub kernel: Option<&'a str>,
    /// The root as an initramfs, for booting from RAM.
    pub initramfs: bool,
    /// The root as what `hide install` installs.
    pub payload: Option<PayloadParts<'a>>,
    /// The image's version, an integer that only goes up: written to
    /// os-release as IMAGE_VERSION, and into the UKI's name, which is how
    /// the boot manager puts the newest deployment first.
    pub version: u64,
}

pub fn assemble(
    set: &RecipeSet,
    layout: &Layout,
    hashes: &BTreeMap<String, InputHash>,
    name: &str,
    outputs: &Outputs,
) -> Result<()> {
    let output = outputs.dir;
    let mut layers = Vec::new();
    for member in set.with_run_closure(&[name])? {
        let recipe = &set.get(&member)?.recipe;
        // Earlier stages exist to build stage 2. Nothing they made may ship.
        if recipe.stage() != Stage::Two {
            bail!(
                "{member} is stage {} and is in {name}'s run closure; an image is stage 2 only",
                recipe.stage()
            );
        }
        let hash = hashes
            .get(&member)
            .ok_or_else(|| anyhow!("no hash for {member}"))?;
        layers.push(output::Layer {
            name: member,
            path: layout.output(hash, recipe),
            stage: 2,
        });
    }
    let conflicts = output::conflicts(&layers)?;
    if !conflicts.is_empty() {
        for c in &conflicts {
            eprintln!(
                "  /{} is provided by both {} and {}",
                c.path.display(),
                c.first,
                c.second
            );
        }
        bail!("{name}: {} path(s) provided twice", conflicts.len());
    }

    let root = layout.image_root(name);
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(&root)?;
    fs::create_dir_all(output)?;
    for layer in &layers {
        // cp -a, because it keeps symlinks, modes and timestamps exactly as
        // the store has them, and the store is what was verified.
        run(Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", layer.path.display()))
            .arg(&root))
        .with_context(|| format!("copying {}", layer.name))?;
    }

    for (dir, mode) in ROOT_DIRECTORIES {
        let path = root.join(dir);
        if !path.exists() {
            fs::create_dir(&path)?;
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(*mode))?;
        }
    }

    // The image's identity, in the os-release the system and the UKI read.
    let os_release = root.join("usr/lib/os-release");
    let mut release = fs::read_to_string(&os_release)
        .with_context(|| format!("{name} has no /usr/lib/os-release"))?;
    release.push_str(&format!(
        "IMAGE_ID=hideos-{name}\nIMAGE_VERSION={}\n",
        outputs.version
    ));
    fs::write(&os_release, release)?;

    let image = &set.get(name)?.recipe.image;
    let excluded = exclude(&root, image)?;
    let stripped = strip(&root)?;
    let missing = missing_libraries(&root)?;
    if !missing.is_empty() {
        for (file, library) in &missing {
            eprintln!("  /{file} needs {library}, which is not in the image");
        }
        bail!(
            "{name}: {} unresolved shared library dependencies",
            missing.len()
        );
    }
    println!(
        "  root    {excluded} paths excluded, {stripped} ELF files stripped, every DT_NEEDED found"
    );

    // Sorted names, fixed owner, reproducible mode: the same closure gives
    // the same archive, byte for byte.
    if outputs.initramfs {
        let archive = output.join("initramfs.cpio");
        let file = fs::File::create(&archive)?;
        run(Command::new("sh")
            .arg("-c")
            .arg("find . -print0 | LC_ALL=C sort -z | cpio --null --create --format=newc --reproducible --owner=0:0 --quiet")
            .current_dir(&root)
            .stdout(file))
        .context("writing the initramfs")?;
        println!("  image   {} ({} layers)", archive.display(), layers.len());
    }

    if let Some(parts) = &outputs.payload {
        write_payload(set, layout, hashes, name, &root, outputs, parts)?;
    }

    if let Some(kernel) = outputs.kernel {
        let recipe = &set.get(kernel)?.recipe;
        let hash = hashes
            .get(kernel)
            .ok_or_else(|| anyhow!("no hash for {kernel}"))?;
        let modules = layout.output(hash, recipe).join("usr/lib/modules");
        let release = fs::read_dir(&modules)
            .with_context(|| format!("{kernel} installed no {}", modules.display()))?
            .next()
            .ok_or_else(|| anyhow!("{kernel} installed no kernel"))??;
        let vmlinuz = release.path().join("vmlinuz");
        fs::copy(&vmlinuz, output.join("vmlinuz"))
            .with_context(|| format!("copying {}", vmlinuz.display()))?;
        println!(
            "  kernel  {} ({})",
            output.join("vmlinuz").display(),
            release.file_name().to_string_lossy()
        );
    }
    Ok(())
}

/// The top-level directories of a hideOS root, made here because no recipe
/// can: a build's sandbox already has every one of them — as a mount point,
/// or from the build root below it — so a recipe's `mkdir` changes nothing
/// and its output never has them. On a read-only root, a missing mount point
/// is a boot that cannot mount /dev.
const ROOT_DIRECTORIES: &[(&str, u32)] = &[
    ("dev", 0o755),
    ("proc", 0o555),
    ("sys", 0o555),
    ("run", 0o755),
    ("tmp", 0o1777),
    ("etc", 0o755),
    ("var", 0o755),
    ("home", 0o755),
    ("root", 0o700),
    ("mnt", 0o755),
    ("hideos", 0o755),
];

/// Removes everything the image recipe excludes. Returns how many paths
/// went; a directory counts once, however much was in it.
fn exclude(root: &Path, image: &hideforge_recipe::Image) -> Result<usize> {
    let mut removed = 0;
    for (relative, meta) in output::walk(root)? {
        let path = root.join(&relative);
        // Already gone with a directory removed earlier in this walk.
        if fs::symlink_metadata(&path).is_err() {
            continue;
        }
        if image.excludes(&relative.to_string_lossy()) {
            if meta.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

/// Every regular file under `root` that starts with the ELF magic.
pub fn elf_files(root: &Path) -> Result<Vec<String>> {
    use std::io::Read;
    let mut files = Vec::new();
    for (relative, meta) in output::walk(root)? {
        if !meta.is_file() {
            continue;
        }
        let mut magic = [0u8; 4];
        let is_elf = fs::File::open(root.join(&relative))
            .and_then(|mut f| f.read_exact(&mut magic))
            .is_ok()
            && magic == *b"\x7fELF";
        if is_elf {
            files.push(relative.to_string_lossy().into_owned());
        }
    }
    Ok(files)
}

/// Strips debug information from every ELF file in the image. The store keeps
/// it: an output is what was built, with everything needed to debug it, and
/// an image is what ships. llvm-strip, because one binary handles every
/// architecture hideOS builds for.
fn strip(root: &Path) -> Result<usize> {
    let files = elf_files(root)?;
    for chunk in files.chunks(200) {
        run(Command::new("llvm-strip")
            .arg("--strip-debug")
            .args(chunk)
            .current_dir(root))
        .context("stripping")?;
    }
    Ok(files.len())
}

/// Every (file, library) where an ELF file in the image names a shared
/// library in DT_NEEDED that the image does not contain.
fn missing_libraries(root: &Path) -> Result<Vec<(String, String)>> {
    unresolved_libraries(root, &[root.to_path_buf()])
}

/// Every (file, library) where an ELF file under `scan` names a shared
/// library in DT_NEEDED that none of `roots` contains. Each root is a tree
/// laid out from `/`. Libraries are looked for where the dynamic linker looks:
/// the file's own RUNPATH or RPATH, with `$ORIGIN` as its directory, then
/// /usr/lib, which /lib and /lib64 point to.
pub fn unresolved_libraries(
    scan: &Path,
    roots: &[std::path::PathBuf],
) -> Result<Vec<(String, String)>> {
    let mut missing = Vec::new();
    for file in elf_files(scan)? {
        let out = Command::new("readelf")
            .args(["--dynamic", "--wide"])
            .arg(&file)
            .current_dir(scan)
            .output()
            .context("running readelf")?;
        let text = String::from_utf8_lossy(&out.stdout);
        let bracketed = |line: &str| {
            line.split('[')
                .nth(1)
                .and_then(|s| s.split(']').next())
                .map(str::to_owned)
        };
        let origin = Path::new(&file)
            .parent()
            .map(|p| format!("/{}", p.display()))
            .unwrap_or_else(|| "/".to_owned());
        let mut search: Vec<String> = text
            .lines()
            .filter(|l| l.contains("(RUNPATH)") || l.contains("(RPATH)"))
            .filter_map(bracketed)
            .flat_map(|paths| {
                paths
                    .split(':')
                    .map(|p| p.replace("$ORIGIN", &origin).replace("${ORIGIN}", &origin))
                    .collect::<Vec<_>>()
            })
            .collect();
        search.push("/usr/lib".to_owned());
        for library in text
            .lines()
            .filter(|l| l.contains("(NEEDED)"))
            .filter_map(bracketed)
        {
            let found = roots.iter().any(|root| {
                search.iter().any(|dir| {
                    let path = root.join(dir.trim_start_matches('/')).join(&library);
                    // A symlink may point into another layer: an absolute
                    // target is resolved against each root, not against /.
                    fs::symlink_metadata(&path).is_ok()
                })
            });
            if !found {
                missing.push((file.clone(), library));
            }
        }
    }
    Ok(missing)
}

/// What a payload needs beyond the root: the kernel to boot it with, and the
/// recipe whose output holds hidestage.
pub struct PayloadParts<'a> {
    pub kernel: &'a str,
    pub initrd: &'a str,
    pub arch: hideforge_recipe::Arch,
    /// A directory with `db.key` and `db.crt`: the key the firmware's db
    /// trusts. Every EFI binary on the ESP is signed with it. `None`
    /// leaves them unsigned, for firmware without Secure Boot.
    pub sign: Option<&'a Path>,
}

/// The system as `hide install` takes it, archived as `payload.tar`:
///
/// ```text
/// repo/          the root as a composefs repository: objects and the EROFS image
/// etc/           the image's /etc, the factory state of the writable /etc
/// esp/           systemd-boot, and the UKI that boots this image
/// image.digest   sha256:<the image's fs-verity digest>
/// ```
///
/// The UKI's command line carries the digest, so it boots this image and no
/// other. Staged in the work directory; only the archive, the digest and the
/// UKI go to `output`.
#[cfg(target_os = "linux")]
fn write_payload(
    set: &RecipeSet,
    layout: &Layout,
    hashes: &BTreeMap<String, InputHash>,
    name: &str,
    root: &Path,
    outputs: &Outputs,
    parts: &PayloadParts,
) -> Result<()> {
    let output = outputs.dir;
    let version = outputs.version;
    let output_of = |recipe: &str| -> Result<std::path::PathBuf> {
        let hash = hashes
            .get(recipe)
            .ok_or_else(|| anyhow!("no hash for {recipe}"))?;
        Ok(layout.output(hash, &set.get(recipe)?.recipe))
    };

    let stage = layout.image_root(&format!("{name}.payload"));
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;

    let digest = crate::payload::write(root, &stage.join("repo"), name)?;
    fs::write(stage.join("image.digest"), format!("sha256:{digest}\n"))?;
    fs::write(output.join("image.digest"), format!("sha256:{digest}\n"))?;
    run(Command::new("cp")
        .arg("-a")
        .arg(root.join("etc"))
        .arg(stage.join("etc")))
    .context("copying the factory /etc")?;

    // The initrd: hidestage as /init, and /dev/console, without which the
    // kernel gives init no stdout and hidestage's messages go nowhere.
    let initrd_root = stage.join("initrd");
    fs::create_dir_all(initrd_root.join("dev"))?;
    fs::copy(
        output_of(parts.initrd)?.join("usr/lib/hideos/hidestage"),
        initrd_root.join("init"),
    )
    .with_context(|| format!("{} installs no usr/lib/hideos/hidestage", parts.initrd))?;
    rustix::fs::mknodat(
        rustix::fs::CWD,
        initrd_root.join("dev/console"),
        rustix::fs::FileType::CharacterDevice,
        rustix::fs::Mode::from_raw_mode(0o600),
        rustix::fs::makedev(5, 1),
    )
    .context("creating dev/console in the initrd")?;
    let initrd = stage.join("initrd.cpio");
    run(Command::new("sh")
        .arg("-c")
        .arg("find . -print0 | LC_ALL=C sort -z | cpio --null --create --format=newc --reproducible --owner=0:0 --quiet")
        .current_dir(&initrd_root)
        .stdout(fs::File::create(&initrd)?))
    .context("writing the initrd")?;

    // The kernel, from the kernel recipe's output.
    let modules = output_of(parts.kernel)?.join("usr/lib/modules");
    let release = fs::read_dir(&modules)?
        .next()
        .ok_or_else(|| anyhow!("{} installed no kernel", parts.kernel))??;
    let vmlinuz = release.path().join("vmlinuz");

    // The serial console last: it becomes /dev/console, which is where
    // hidestage, oxinit and the tests read.
    let (stub_name, console, boot_efi) = match parts.arch {
        hideforge_recipe::Arch::X86_64 => (
            "linuxx64.efi.stub",
            "console=tty0 console=ttyS0",
            "BOOTX64.EFI",
        ),
        hideforge_recipe::Arch::Aarch64 => (
            "linuxaa64.efi.stub",
            "console=tty0 console=ttyAMA0",
            "BOOTAA64.EFI",
        ),
    };
    // panic=10: a kernel that panics reboots, and the boot manager counts
    // the attempt against this deployment, rather than the machine sitting
    // on a panic screen with no way back.
    let cmdline = format!("{console} hideos.image=sha256:{digest} panic=10");
    // hide's deployment names; see hide::deployment.
    let uki_name = format!(
        "hideos-{name}-{version}-{}.efi",
        digest.get(..12).unwrap_or(&digest)
    );
    let esp = stage.join("esp");
    fs::create_dir_all(esp.join("EFI/Linux"))?;
    crate::uki::build(
        &Path::new(BOOT_EFI_DIR).join(stub_name),
        &root.join("usr/lib/os-release"),
        &cmdline,
        &initrd,
        &vmlinuz,
        &esp.join("EFI/Linux").join(&uki_name),
    )?;

    // systemd-boot, until hideBoot: it finds UKIs in EFI/Linux by itself.
    let loader = Path::new(BOOT_EFI_DIR).join(format!(
        "systemd-boot{}.efi",
        if parts.arch == hideforge_recipe::Arch::X86_64 {
            "x64"
        } else {
            "aa64"
        }
    ));
    fs::create_dir_all(esp.join("EFI/BOOT"))?;
    fs::create_dir_all(esp.join("EFI/systemd"))?;
    fs::create_dir_all(esp.join("loader"))?;
    fs::copy(&loader, esp.join("EFI/BOOT").join(boot_efi))
        .with_context(|| format!("copying {}", loader.display()))?;
    fs::copy(
        &loader,
        esp.join("EFI/systemd")
            .join(loader.file_name().unwrap_or_default()),
    )?;
    fs::write(esp.join("loader/loader.conf"), "timeout 3\n")?;

    // Signed last, once nothing on the ESP will change. The UKI is the one
    // that matters: its command line carries the image's digest, so the
    // signature is what makes the seal reach the firmware. systemd-boot is
    // signed because the firmware will not start it otherwise.
    match parts.sign {
        Some(keys) => {
            for file in [
                esp.join("EFI/Linux").join(&uki_name),
                esp.join("EFI/BOOT").join(boot_efi),
                esp.join("EFI/systemd")
                    .join(loader.file_name().unwrap_or_default()),
            ] {
                sign_efi(keys, &file)?;
            }
            println!("  signed  with {}", keys.join("db.crt").display());
        }
        None => println!("  signed  no: EFI binaries left unsigned"),
    }
    fs::copy(
        esp.join("EFI/Linux").join(&uki_name),
        output.join(&uki_name),
    )?;

    let tar = fs::File::create(output.join("payload.tar"))?;
    run(Command::new("tar")
        .args([
            "--create",
            "--sort=name",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
        ])
        .args(["--mtime=@0", "--directory"])
        .arg(&stage)
        .args(["repo", "etc", "esp", "image.digest"])
        .stdout(tar))
    .context("archiving the payload")?;
    println!(
        "  payload {} (sha256:{digest})",
        output.join("payload.tar").display()
    );
    println!("  uki     {}", output.join(&uki_name).display());
    Ok(())
}

/// Signs an EFI binary in place with `keys`/db.key, then checks the
/// signature against `keys`/db.crt, so that a key and certificate that do
/// not belong together fail here rather than at the firmware.
#[cfg(target_os = "linux")]
fn sign_efi(keys: &Path, file: &Path) -> Result<()> {
    let signed = file.with_extension("signed");
    run(Command::new("sbsign")
        .arg("--key")
        .arg(keys.join("db.key"))
        .arg("--cert")
        .arg(keys.join("db.crt"))
        .arg("--output")
        .arg(&signed)
        .arg(file))
    .with_context(|| format!("signing {}", file.display()))?;
    fs::rename(&signed, file)?;
    run(Command::new("sbverify")
        .arg("--cert")
        .arg(keys.join("db.crt"))
        .arg(file)
        .stdout(std::process::Stdio::null()))
    .with_context(|| format!("verifying the signature of {}", file.display()))
}

/// Where the builder's systemd-boot package puts its EFI binaries. The stub
/// and the boot manager come from the builder until hideBoot (H7) replaces
/// the boot manager; see ARCHITECTURE.md, "Boot chain".
#[cfg(target_os = "linux")]
const BOOT_EFI_DIR: &str = "/usr/lib/systemd/boot/efi";

#[cfg(not(target_os = "linux"))]
fn write_payload(
    _: &RecipeSet,
    _: &Layout,
    _: &BTreeMap<String, InputHash>,
    _: &str,
    _: &Path,
    _: &Outputs,
    _: &PayloadParts,
) -> Result<()> {
    bail!("payloads are written on Linux, in the builder")
}

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {command:?}"))?;
    if !status.success() {
        bail!("{command:?} failed: {status}");
    }
    Ok(())
}
