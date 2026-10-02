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
pub fn assemble(
    set: &RecipeSet,
    layout: &Layout,
    hashes: &BTreeMap<String, InputHash>,
    name: &str,
    kernel: Option<&str>,
    output: &Path,
    payload: bool,
) -> Result<()> {
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
    let archive = output.join("initramfs.cpio");
    let file = fs::File::create(&archive)?;
    run(Command::new("sh")
        .arg("-c")
        .arg("find . -print0 | LC_ALL=C sort -z | cpio --null --create --format=newc --reproducible --owner=0:0 --quiet")
        .current_dir(&root)
        .stdout(file))
    .context("writing the initramfs")?;
    println!("  image   {} ({} layers)", archive.display(), layers.len());

    if payload {
        write_payload(layout, name, &root, output)?;
    }

    if let Some(kernel) = kernel {
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

/// The same root as a composefs repository, archived as `payload.tar` for
/// `hide install` to read, and its image digest as `image.digest`. The
/// repository is written in the work directory: objects are named by digest
/// and would survive a macOS checkout, but there is no reason to make the
/// checkout hold a second copy of the system.
#[cfg(target_os = "linux")]
fn write_payload(layout: &Layout, name: &str, root: &Path, output: &Path) -> Result<()> {
    let repo = layout.image_root(&format!("{name}.repo"));
    if repo.exists() {
        fs::remove_dir_all(&repo)?;
    }
    let digest = crate::payload::write(root, &repo, name)?;
    fs::write(output.join("image.digest"), format!("sha256:{digest}\n"))?;
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
        .arg(&repo)
        .arg(".")
        .stdout(tar))
    .context("archiving the payload")?;
    println!(
        "  payload {} (sha256:{digest})",
        output.join("payload.tar").display()
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn write_payload(_: &Layout, _: &str, _: &Path, _: &Path) -> Result<()> {
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
