//! hideforge: builds hideOS from recipes. See `docs/HIDEFORGE.md`.
//!
//! ```text
//! hideforge [--recipes DIR] [--work DIR] [--arch ARCH] COMMAND
//!
//!   list                  every recipe
//!   order NAME...         what building NAME builds, in order
//!   hash NAME [--explain] NAME's input hash, and what went into it
//!   fetch NAME...         download and verify sources for NAME and its inputs
//!   build NAME... [--keep-failed] [--keep-going]
//!                         --keep-going: after a failure, build everything
//!                         that does not need what failed, then report
//!   image NAME --output DIR --kernel NAME [--payload --initrd NAME]
//!                         build NAME and assemble its run closure into an
//!                         initramfs, with the kernel next to it; --payload
//!                         also writes what hide install installs: the
//!                         composefs repository, the UKI, the ESP;
//!                         --no-initramfs skips the initramfs, for images
//!                         that boot only from a disk; --sign DIR signs
//!                         the ESP's EFI binaries with DIR/db.key
//! ```
//!
//! Building needs Linux, root and a writable work directory, which is what the
//! builder container provides: `cargo xtask forge -- build NAME`.

// `deny`, not `forbid`: `sys` is the one module that relaxes it, and a second
// `unsafe` block anywhere else stops compiling.
#![deny(unsafe_code)]
// Building is Linux-only, so off Linux everything only the build uses is
// unused. Lints still run in full on Linux, in the builder.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod fetch;
mod image;
mod layout;
mod output;
#[cfg(target_os = "linux")]
mod payload;
#[cfg(target_os = "linux")]
mod sandbox;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys;
#[cfg(target_os = "linux")]
mod uki;
mod workspace;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use hideforge_recipe::{Arch, HashContext, InputHash, RecipeSet};

use crate::layout::Layout;

struct Options {
    recipes: PathBuf,
    work: PathBuf,
    arch: Arch,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    // The sandbox's own entry points come first and take no global options:
    // they are hideforge re-executing itself, not a person typing.
    match args.first().map(String::as_str) {
        #[cfg(target_os = "linux")]
        Some("__sandbox") => return sandbox::outer(args.get(1..).unwrap_or_default()),
        #[cfg(target_os = "linux")]
        Some("__sandbox-init") => return sandbox::init(args.get(1..).unwrap_or_default()),
        _ => {}
    }

    let mut options = Options {
        recipes: PathBuf::from("recipes"),
        work: std::env::var_os("HIDEFORGE_WORK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/work")),
        arch: std::env::consts::ARCH
            .parse()
            .map_err(|e: String| anyhow!(e))?,
    };
    let mut rest = args;
    loop {
        match rest {
            [flag, value, tail @ ..] if flag == "--recipes" => {
                options.recipes = value.into();
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--work" => {
                options.work = value.into();
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--arch" => {
                options.arch = value.parse().map_err(|e: String| anyhow!(e))?;
                rest = tail;
            }
            _ => break,
        }
    }

    let (command, rest) = rest
        .split_first()
        .ok_or_else(|| anyhow!("no command; try `hideforge list`"))?;
    let set = RecipeSet::load(&options.recipes)
        .with_context(|| format!("loading recipes from {}", options.recipes.display()))?;
    let layout = Layout::new(&options.work);
    // The workspace snapshot is needed only for hashing, and archiving it
    // needs GNU tar: on Linux, in the builder.
    let workspace = if cfg!(target_os = "linux") && command != "list" && command != "order" {
        // `recipes` has an empty parent, which git -C does not take.
        let root = match options.recipes.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => std::path::Path::new("."),
        };
        workspace::snapshot(root, &layout)?
    } else {
        None
    };
    let context = HashContext {
        arch: options.arch,
        host_id: std::env::var("HIDEFORGE_HOST_ID").ok(),
        workspace,
    };
    let parsed = parse_args(rest)?;
    let names: Vec<&str> = parsed.names.iter().map(String::as_str).collect();
    let flag = |name: &str| parsed.flags.iter().any(|f| f == name);

    match command.as_str() {
        "list" => {
            for name in set.names() {
                let recipe = &set.get(name)?.recipe;
                println!(
                    "{:<32} {:<14} stage {}  {}",
                    name,
                    recipe.package.version,
                    recipe.stage(),
                    recipe.package.description
                );
            }
        }
        "order" => {
            let targets = set.with_run_closure(&needs_names(&names)?)?;
            let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
            for name in set.build_order(&targets)? {
                println!("{name}");
            }
        }
        "hash" => {
            let [name] = names.as_slice() else {
                bail!("usage: hideforge hash NAME [--explain]");
            };
            if flag("--explain") {
                print!("{}", set.explain_hash(name, &context)?);
            } else {
                let hashes = set.input_hashes(&[name], &context)?;
                let hash = hashes
                    .get(*name)
                    .ok_or_else(|| anyhow!("no hash for {name}"))?;
                println!("{hash}");
            }
        }
        "fetch" => {
            let targets = set.with_run_closure(&needs_names(&names)?)?;
            let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
            for name in set.build_order(&targets)? {
                fetch::fetch(&layout, set.get(&name)?, options.arch)?;
            }
        }
        "build" => {
            let targets = set.with_run_closure(&needs_names(&names)?)?;
            let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
            let hashes = set.input_hashes(&targets, &context)?;
            let order = set.build_order(&targets)?;
            build(
                &set,
                &layout,
                &context,
                &hashes,
                &order,
                BuildOptions {
                    keep_failed: flag("--keep-failed"),
                    keep_going: flag("--keep-going"),
                },
            )?;
        }
        "image" => {
            let usage = "usage: hideforge image NAME --output DIR --kernel NAME \
                         [--payload --initrd NAME [--sign DIR]] [--no-initramfs]";
            let [name] = names.as_slice() else {
                bail!("{usage}");
            };
            let output = parsed.value("--output").ok_or_else(|| anyhow!("{usage}"))?;
            let kernel = parsed.value("--kernel");
            let initrd = parsed.value("--initrd");
            let payload = match (flag("--payload"), kernel, initrd) {
                (false, _, _) => None,
                (true, Some(kernel), Some(initrd)) => Some(image::PayloadParts {
                    kernel,
                    initrd,
                    arch: options.arch,
                    sign: parsed.value("--sign").map(std::path::Path::new),
                }),
                (true, _, _) => bail!("--payload needs --kernel and --initrd\n{usage}"),
            };
            let mut wanted = vec![*name];
            wanted.extend(kernel);
            wanted.extend(initrd);
            let targets = set.with_run_closure(&wanted)?;
            let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
            let hashes = set.input_hashes(&targets, &context)?;
            let order = set.build_order(&targets)?;
            build(
                &set,
                &layout,
                &context,
                &hashes,
                &order,
                BuildOptions::default(),
            )?;
            let outputs = image::Outputs {
                dir: output.as_ref(),
                kernel,
                initramfs: !flag("--no-initramfs"),
                payload,
            };
            image::assemble(&set, &layout, &hashes, name, &outputs)?;
        }
        other => bail!("unknown command `{other}`"),
    }
    Ok(0)
}

/// A command's arguments: recipe names, `--flag`s, and `--option VALUE`s.
struct Args {
    names: Vec<String>,
    flags: Vec<String>,
    values: Vec<(String, String)>,
}

impl Args {
    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Options that take a value. Everything else starting with `--` is a flag.
const VALUE_OPTIONS: &[&str] = &["--output", "--kernel", "--initrd", "--sign"];

fn parse_args(args: &[String]) -> Result<Args> {
    let mut parsed = Args {
        names: Vec::new(),
        flags: Vec::new(),
        values: Vec::new(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if VALUE_OPTIONS.contains(&arg.as_str()) {
            let value = iter.next().ok_or_else(|| anyhow!("{arg} needs a value"))?;
            parsed.values.push((arg.clone(), value.clone()));
        } else if arg.starts_with("--") {
            parsed.flags.push(arg.clone());
        } else {
            parsed.names.push(arg.clone());
        }
    }
    Ok(parsed)
}

fn needs_names<'a>(names: &[&'a str]) -> Result<Vec<&'a str>> {
    if names.is_empty() {
        bail!("name at least one recipe");
    }
    Ok(names.to_vec())
}

/// How `build` treats failures.
#[derive(Clone, Copy, Default)]
struct BuildOptions {
    /// Keep a failed build's directory, to look at.
    keep_failed: bool,
    /// Go on with whatever does not need what failed.
    keep_going: bool,
}

#[cfg(not(target_os = "linux"))]
fn build(
    _: &RecipeSet,
    _: &Layout,
    _: &HashContext,
    _: &BTreeMap<String, InputHash>,
    _: &[String],
    _: BuildOptions,
) -> Result<()> {
    bail!("building needs Linux; run it in the builder: cargo xtask forge -- build NAME")
}

#[cfg(target_os = "linux")]
fn build(
    set: &RecipeSet,
    layout: &Layout,
    context: &HashContext,
    hashes: &BTreeMap<String, InputHash>,
    order: &[String],
    options: BuildOptions,
) -> Result<()> {
    let exe = sandbox::ExeCopy::new(layout.exe_copy())?;
    // Failed, or not built because something they need failed.
    let mut failed: Vec<String> = Vec::new();
    for name in order {
        if options.keep_going {
            let inputs = set.sandbox_inputs(name)?;
            if let Some(cause) = failed.iter().find(|f| inputs.contains(*f)) {
                println!("  skip    {name}: needs {cause}");
                failed.push(name.clone());
                continue;
            }
        }
        if let Err(error) = build_one(
            set,
            layout,
            context,
            hashes,
            name,
            options.keep_failed,
            &exe,
        ) {
            if !options.keep_going {
                return Err(error);
            }
            eprintln!("error: {error:#}");
            failed.push(name.clone());
        }
    }
    if !failed.is_empty() {
        bail!(
            "{} recipe(s) not built: {}",
            failed.len(),
            failed.join(", ")
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn build_one(
    set: &RecipeSet,
    layout: &Layout,
    context: &HashContext,
    hashes: &BTreeMap<String, InputHash>,
    name: &str,
    keep_failed: bool,
    exe: &sandbox::ExeCopy,
) -> Result<()> {
    use std::fs;
    use std::time::Instant;

    use hideforge_recipe::Environment;

    let entry = set.get(name)?;
    let recipe = &entry.recipe;
    let hash = hashes
        .get(name)
        .ok_or_else(|| anyhow!("no hash for {name}"))?;
    let output = layout.output(hash, recipe);
    if output.is_dir() {
        println!("  cached  {}", layout::store_name(hash, recipe));
        return Ok(());
    }
    println!("  build   {}", layout::store_name(hash, recipe));
    let started = Instant::now();

    fetch::fetch(layout, entry, context.arch)?;

    let dirs = layout.build(hash);
    if dirs.base().exists() {
        fs::remove_dir_all(dirs.base())?;
    }
    for dir in [
        dirs.src(),
        dirs.home(),
        dirs.upper(),
        dirs.overlay_work(),
        dirs.root(),
        dirs.skeleton(),
    ] {
        fs::create_dir_all(dir)?;
    }
    if recipe.build.environment == Environment::Target {
        for mount_point in [
            "proc",
            "sys",
            "dev",
            "tmp",
            "build/src",
            "build/home",
            ".old",
        ] {
            fs::create_dir_all(dirs.skeleton().join(mount_point))?;
        }
    }

    // Lower layers: every sandbox input's output, which the build order
    // guarantees exists by now.
    let mut layers = Vec::new();
    for input in set.sandbox_inputs(name)? {
        let input_recipe = &set.get(&input)?.recipe;
        let input_hash = hashes
            .get(&input)
            .ok_or_else(|| anyhow!("no hash for {input}"))?;
        let path = layout.output(input_hash, input_recipe);
        if !path.is_dir() {
            bail!(
                "{name} needs {input}, whose output {} is missing",
                path.display()
            );
        }
        layers.push(output::Layer {
            name: input,
            path,
            stage: input_recipe.stage().number(),
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
        bail!(
            "{name}: {} path(s) provided twice by its inputs",
            conflicts.len()
        );
    }
    output::overlay_order(&mut layers);
    let mut lowers: Vec<PathBuf> = layers.iter().map(|layer| layer.path.clone()).collect();
    lowers.push(dirs.skeleton());

    let epoch = fetch::prepare(
        layout,
        entry,
        &dirs.src(),
        context.arch,
        context.workspace.as_deref(),
    )?;
    fs::write(dirs.script(), &recipe.build.script)?;

    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    let sysroot = match recipe.build.environment {
        Environment::Host => "/sysroot",
        Environment::Target => "/",
    };
    let spec = sandbox::Spec {
        environment: recipe.build.environment,
        root: dirs.root(),
        upper: dirs.upper(),
        overlay_work: dirs.overlay_work(),
        lowers,
        src: dirs.src(),
        home: dirs.home(),
        script: "/build/home/.hideforge-build.sh".to_owned(),
        vars: vec![
            ("JOBS".to_owned(), jobs.to_string()),
            ("ARCH".to_owned(), context.arch.as_str().to_owned()),
            ("TARGET".to_owned(), context.arch.target_triple()),
            ("SYSROOT".to_owned(), sysroot.to_owned()),
            ("SOURCE_DATE_EPOCH".to_owned(), epoch.to_string()),
            (
                "FILES".to_owned(),
                format!("/build/src/{}", fetch::FILES_DIR),
            ),
        ],
    };

    fs::create_dir_all(layout.logs())?;
    let log = layout.log(hash, recipe);
    let status = sandbox::run(&spec, &log, exe)?;
    if !status.success() {
        let text = fs::read_to_string(&log).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        for line in tail.iter().rev() {
            eprintln!("  | {line}");
        }
        if !keep_failed {
            let _ = fs::remove_dir_all(dirs.base());
        }
        bail!("{name} failed ({status}); full log: {}", log.display());
    }

    output::remove_image_indexes(&dirs.upper())?;
    let violations = output::check_upper(&dirs.upper(), &layers, recipe.stage().number())?;
    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("  {violation}");
        }
        if !keep_failed {
            let _ = fs::remove_dir_all(dirs.base());
        }
        bail!("{name} changed files its inputs provide; see above");
    }
    // A stage-2 output may only link what stage 2 provides. The build root
    // has stage 0 and 1 underneath, so a configure script that finds a
    // library there links it, the build works, and the result fails on a
    // machine where only stage 2 exists. Found here, not when an image is
    // assembled.
    if recipe.stage() == hideforge_recipe::Stage::Two {
        let mut roots = vec![dirs.upper()];
        roots.extend(
            layers
                .iter()
                .filter(|l| l.stage == 2)
                .map(|l| l.path.clone()),
        );
        let unresolved = image::unresolved_libraries(&dirs.upper(), &roots)?;
        if !unresolved.is_empty() {
            for (file, library) in &unresolved {
                eprintln!("  /{file} needs {library}, which no stage-2 output provides");
            }
            if !keep_failed {
                let _ = fs::remove_dir_all(dirs.base());
            }
            bail!("{name} links libraries only an earlier stage provides; see above");
        }
    }
    output::remove_whiteouts(&dirs.upper())?;
    sandbox::strip_overlay_xattrs(&dirs.upper())?;
    sandbox::clamp_mtimes(&dirs.upper(), epoch)?;

    fs::create_dir_all(layout.store())?;
    fs::rename(dirs.upper(), &output)?;
    fs::remove_dir_all(dirs.base())?;
    println!(
        "  done    {} in {:.1}s",
        layout::store_name(hash, recipe),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
