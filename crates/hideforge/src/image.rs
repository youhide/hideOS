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

/// Merges the run closure of `name` into `output/root`, archives it as
/// `output/initramfs.cpio`, and copies `kernel`'s image to `output/vmlinuz`.
pub fn assemble(
    set: &RecipeSet,
    layout: &Layout,
    hashes: &BTreeMap<String, InputHash>,
    name: &str,
    kernel: Option<&str>,
    output: &Path,
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

    let root = output.join("root");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(&root)?;
    for layer in &layers {
        // cp -a, because it keeps symlinks, modes and timestamps exactly as
        // the store has them, and the store is what was verified.
        run(Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", layer.path.display()))
            .arg(&root))
        .with_context(|| format!("copying {}", layer.name))?;
    }

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

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {command:?}"))?;
    if !status.success() {
        bail!("{command:?} failed: {status}");
    }
    Ok(())
}
