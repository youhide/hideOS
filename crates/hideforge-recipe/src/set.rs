//! The recipe tree: loading, cross-recipe checks, sandbox inputs, build
//! order, and input hashes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::hash::{Hasher, sha256_hex};
use crate::{Environment, Error, HashContext, InputHash, Recipe};

/// One loaded recipe, with what was read from disk alongside it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub recipe: Recipe,
    /// The recipe file.
    pub path: PathBuf,
    /// The directory holding its patches: next to the file, named after its
    /// stem.
    pub files_dir: PathBuf,
    /// SHA-256 of the recipe file, byte for byte.
    file_digest: String,
    /// SHA-256 of each patch, in the order the recipe applies them.
    patch_digests: Vec<(String, String)>,
}

/// Every recipe under one directory, checked against each other.
#[derive(Debug, Clone)]
pub struct RecipeSet {
    entries: BTreeMap<String, Entry>,
}

impl RecipeSet {
    /// Reads every `.toml` file under `root`, recursively, and checks the
    /// whole set: unique names, dependencies that exist, stage order, and no
    /// cycles. Patches are read here too, so that a missing patch is found
    /// when the tree is loaded rather than halfway through a build.
    pub fn load(root: &Path) -> Result<RecipeSet, Error> {
        let mut files = Vec::new();
        collect_toml(root, &mut files)?;
        files.sort();

        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for path in files {
            let bytes = fs::read(&path).map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
            let text = String::from_utf8(bytes.clone()).map_err(|_| Error::Parse {
                path: path.clone(),
                message: "not UTF-8".to_owned(),
            })?;
            let recipe = Recipe::parse(&text, &path)?;

            let files_dir = path.with_extension("");
            let mut patch_digests = Vec::new();
            for patch in &recipe.build.patches {
                let patch_path = files_dir.join(patch);
                let patch_bytes = fs::read(&patch_path).map_err(|source| Error::Read {
                    path: patch_path.clone(),
                    source,
                })?;
                patch_digests.push((patch.clone(), sha256_hex(&patch_bytes)));
            }

            let name = recipe.name().to_owned();
            let entry = Entry {
                recipe,
                file_digest: sha256_hex(&bytes),
                patch_digests,
                files_dir,
                path,
            };
            if let Some(first) = entries.get(&name) {
                return Err(Error::DuplicateName {
                    name,
                    first: first.path.clone(),
                    second: entry.path,
                });
            }
            entries.insert(name, entry);
        }

        let set = RecipeSet { entries };
        set.check()?;
        Ok(set)
    }

    pub fn get(&self, name: &str) -> Result<&Entry, Error> {
        self.entries
            .get(name)
            .ok_or_else(|| Error::UnknownRecipe(name.to_owned()))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The recipes whose outputs are layered into `name`'s sandbox: its build
    /// dependencies, and everything those need at run time, recursively.
    /// Sorted, so that layer order — and therefore the sandbox — is the same
    /// on every run.
    pub fn sandbox_inputs(&self, name: &str) -> Result<BTreeSet<String>, Error> {
        let entry = self.get(name)?;
        let mut inputs = BTreeSet::new();
        let mut pending: Vec<&str> = entry
            .recipe
            .depends
            .build
            .iter()
            .map(String::as_str)
            .collect();
        while let Some(next) = pending.pop() {
            if inputs.insert(next.to_owned()) {
                pending.extend(
                    self.get(next)?
                        .recipe
                        .depends
                        .run
                        .iter()
                        .map(String::as_str),
                );
            }
        }
        Ok(inputs)
    }

    /// Everything needed to build `targets`, dependencies before dependents,
    /// targets included. Deterministic: the same set and targets always give
    /// the same order.
    pub fn build_order(&self, targets: &[&str]) -> Result<Vec<String>, Error> {
        let mut order = Vec::new();
        let mut done = BTreeSet::new();
        let mut sorted: Vec<&str> = targets.to_vec();
        sorted.sort_unstable();
        for target in sorted {
            self.visit(target, &mut done, &mut Vec::new(), &mut order)?;
        }
        Ok(order)
    }

    /// Depth-first, post-order over sandbox inputs. `stack` is the current
    /// path, so a cycle is reported as the cycle, not only as its existence.
    fn visit(
        &self,
        name: &str,
        done: &mut BTreeSet<String>,
        stack: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<(), Error> {
        if done.contains(name) {
            return Ok(());
        }
        if let Some(at) = stack.iter().position(|n| n == name) {
            let mut path: Vec<String> = stack.get(at..).unwrap_or_default().to_vec();
            path.push(name.to_owned());
            return Err(Error::Cycle { path });
        }
        stack.push(name.to_owned());
        for input in self.sandbox_inputs(name)? {
            self.visit(&input, done, stack, order)?;
        }
        stack.pop();
        done.insert(name.to_owned());
        order.push(name.to_owned());
        Ok(())
    }

    /// The input hash of every recipe needed for `targets`, keyed by name.
    pub fn input_hashes(
        &self,
        targets: &[&str],
        context: &HashContext,
    ) -> Result<BTreeMap<String, InputHash>, Error> {
        let mut hashes = BTreeMap::new();
        for name in self.build_order(targets)? {
            let (hash, _) = self.hash_one(&name, context, &hashes)?;
            hashes.insert(name, hash);
        }
        Ok(hashes)
    }

    /// The exact text hashed for `name`, for when two hashes differ and
    /// nobody can see why.
    pub fn explain_hash(&self, name: &str, context: &HashContext) -> Result<String, Error> {
        let hashes = self.input_hashes(&[name], context)?;
        let (_, text) = self.hash_one(name, context, &hashes)?;
        Ok(text)
    }

    /// Hashes one recipe whose sandbox inputs are already in `hashes`.
    fn hash_one(
        &self,
        name: &str,
        context: &HashContext,
        hashes: &BTreeMap<String, InputHash>,
    ) -> Result<(InputHash, String), Error> {
        let entry = self.get(name)?;
        let mut hasher = Hasher::new();
        hasher.line("name", name);
        hasher.line("recipe", &entry.file_digest);
        for (patch, digest) in &entry.patch_digests {
            hasher.line("patch", &format!("{patch} {digest}"));
        }
        hasher.line("arch", context.arch.as_str());
        if entry.recipe.build.environment == Environment::Host {
            let host = context
                .host_id
                .as_deref()
                .ok_or_else(|| Error::MissingHostId(name.to_owned()))?;
            hasher.line("host", host);
        }
        // Run dependencies are not here: they do not change what this build
        // produces, only what an image must carry alongside it. Including
        // them would rebuild a library every time a program it calls at run
        // time changed.
        for input in self.sandbox_inputs(name)? {
            let hash = hashes
                .get(&input)
                .ok_or_else(|| Error::UnknownRecipe(input.clone()))?;
            hasher.line("input", &format!("{input} {hash}"));
        }
        Ok(hasher.finish())
    }

    fn check(&self) -> Result<(), Error> {
        for (name, entry) in &self.entries {
            let stage = entry.recipe.stage();
            for dependency in entry.recipe.all_dependencies() {
                let dep = self
                    .entries
                    .get(dependency)
                    .ok_or_else(|| Error::UnknownDependency {
                        recipe: name.clone(),
                        dependency: dependency.to_owned(),
                    })?;
                let dep_stage = dep.recipe.stage();
                if dep_stage > stage || dep_stage.number() + 1 < stage.number() {
                    return Err(Error::StageOrder {
                        recipe: name.clone(),
                        stage: stage.number(),
                        dependency: dependency.to_owned(),
                        dependency_stage: dep_stage.number(),
                    });
                }
            }
        }
        // Every recipe, so a cycle among recipes nothing currently asks for
        // is still found when the tree is loaded.
        let all: Vec<&str> = self.names().collect();
        self.build_order(&all)?;
        Ok(())
    }
}

fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let read = fs::read_dir(dir).map_err(|source| Error::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    for item in read {
        let item = item.map_err(|source| Error::Read {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = item.path();
        let kind = item.file_type().map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        if kind.is_dir() {
            collect_toml(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Arch;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch recipe tree that removes itself.
    struct Tree(PathBuf);

    impl Tree {
        fn new() -> Tree {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "hideforge-recipe-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        /// Writes a minimal recipe. `extra` goes into `[package]`, `build`
        /// and `run` into `[depends]`.
        fn recipe(&self, file: &str, name: &str, extra: &str, build: &[&str], run: &[&str]) {
            let list = |names: &[&str]| {
                names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let text = format!(
                "[package]\nname = \"{name}\"\nversion = \"1\"\ndescription = \"d\"\n\
                 license = \"MIT\"\n{extra}\n\n[depends]\nbuild = [{}]\nrun = [{}]\n\n\
                 [build]\nscript = \"true\"\n",
                list(build),
                list(run)
            );
            let path = self.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn load(&self) -> Result<RecipeSet, Error> {
            RecipeSet::load(&self.0)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn context() -> HashContext {
        HashContext {
            arch: Arch::X86_64,
            host_id: None,
        }
    }

    #[test]
    fn loads_recursively_and_orders_dependencies_first() {
        let tree = Tree::new();
        tree.recipe("base/app.toml", "app", "", &["lib", "tool"], &[]);
        tree.recipe("base/lib.toml", "lib", "", &["tool"], &[]);
        tree.recipe("dev/tool.toml", "tool", "", &[], &[]);
        let set = tree.load().unwrap();
        assert_eq!(set.len(), 3);
        assert_eq!(set.build_order(&["app"]).unwrap(), ["tool", "lib", "app"]);
        assert_eq!(set.build_order(&["lib"]).unwrap(), ["tool", "lib"]);
    }

    #[test]
    fn sandbox_inputs_include_the_run_closure_of_build_dependencies() {
        let tree = Tree::new();
        tree.recipe("app.toml", "app", "", &["compiler"], &["runtime-only"]);
        tree.recipe("compiler.toml", "compiler", "", &[], &["libc"]);
        tree.recipe("libc.toml", "libc", "", &[], &["tzdata"]);
        tree.recipe("tzdata.toml", "tzdata", "", &[], &[]);
        tree.recipe("runtime-only.toml", "runtime-only", "", &[], &[]);
        let set = tree.load().unwrap();
        let inputs: Vec<String> = set.sandbox_inputs("app").unwrap().into_iter().collect();
        // The app's own run dependency is not in its sandbox.
        assert_eq!(inputs, ["compiler", "libc", "tzdata"]);
    }

    #[test]
    fn duplicate_names_name_both_files() {
        let tree = Tree::new();
        tree.recipe("a/zlib.toml", "zlib", "", &[], &[]);
        tree.recipe("b/zlib.toml", "zlib", "", &[], &[]);
        match tree.load().unwrap_err() {
            Error::DuplicateName {
                name,
                first,
                second,
            } => {
                assert_eq!(name, "zlib");
                assert!(first.ends_with("a/zlib.toml"));
                assert!(second.ends_with("b/zlib.toml"));
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn unknown_dependencies_are_found_at_load() {
        let tree = Tree::new();
        tree.recipe("app.toml", "app", "", &["nothing"], &[]);
        assert!(matches!(
            tree.load().unwrap_err(),
            Error::UnknownDependency { dependency, .. } if dependency == "nothing"
        ));
    }

    #[test]
    fn cycles_are_reported_with_their_path() {
        let tree = Tree::new();
        tree.recipe("a.toml", "a", "", &["b"], &[]);
        tree.recipe("b.toml", "b", "", &["c"], &[]);
        // c needs a only at run time, but c is in b's sandbox, so a would be
        // too: still a cycle.
        tree.recipe("c.toml", "c", "", &[], &["a"]);
        match tree.load().unwrap_err() {
            Error::Cycle { path } => assert_eq!(path, ["a", "b", "a"]),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn run_only_cycles_are_allowed() {
        // Two programs that call each other at run time. Neither is in the
        // other's sandbox, so nothing needs the other to be built first.
        let tree = Tree::new();
        tree.recipe("a.toml", "a", "", &[], &["b"]);
        tree.recipe("b.toml", "b", "", &[], &["a"]);
        assert!(tree.load().is_ok());
    }

    #[test]
    fn stages_may_only_look_one_back() {
        let tree = Tree::new();
        tree.recipe("s0.toml", "stage0-gcc", "stage = 0", &[], &[]);
        tree.recipe("s1.toml", "stage1-gcc", "stage = 1", &["stage0-gcc"], &[]);
        tree.recipe("app.toml", "app", "", &["stage1-gcc"], &[]);
        assert!(tree.load().is_ok());

        tree.recipe("bad.toml", "bad", "", &["stage0-gcc"], &[]);
        assert!(matches!(
            tree.load().unwrap_err(),
            Error::StageOrder { recipe, .. } if recipe == "bad"
        ));
    }

    #[test]
    fn earlier_stages_cannot_depend_on_later_ones() {
        let tree = Tree::new();
        tree.recipe("app.toml", "app", "", &[], &[]);
        tree.recipe("s1.toml", "stage1-gcc", "stage = 1", &["app"], &[]);
        assert!(matches!(tree.load().unwrap_err(), Error::StageOrder { .. }));
    }

    #[test]
    fn missing_patches_are_found_at_load() {
        let tree = Tree::new();
        tree.recipe("zlib.toml", "zlib", "", &[], &[]);
        let path = tree.0.join("zlib.toml");
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("[build]", "[build]\npatches = [\"fix.patch\"]");
        fs::write(&path, text).unwrap();
        assert!(
            matches!(tree.load().unwrap_err(), Error::Read { path, .. } if path.ends_with("zlib/fix.patch"))
        );

        fs::create_dir_all(tree.0.join("zlib")).unwrap();
        fs::write(tree.0.join("zlib/fix.patch"), "--- a\n+++ b\n").unwrap();
        assert!(tree.load().is_ok());
    }

    #[test]
    fn hashes_are_stable_and_propagate_to_dependents_only() {
        let tree = Tree::new();
        tree.recipe("app.toml", "app", "", &["lib"], &["helper"]);
        tree.recipe("lib.toml", "lib", "", &[], &[]);
        tree.recipe("helper.toml", "helper", "", &[], &[]);
        tree.recipe("other.toml", "other", "", &[], &[]);

        let all = ["app", "helper", "other"];
        let before = tree.load().unwrap().input_hashes(&all, &context()).unwrap();
        let again = tree.load().unwrap().input_hashes(&all, &context()).unwrap();
        assert_eq!(before, again);

        // A change to lib changes lib and app, nothing else.
        tree.recipe("lib.toml", "lib", "homepage = \"https://x\"", &[], &[]);
        let after = tree.load().unwrap().input_hashes(&all, &context()).unwrap();
        assert_ne!(before["lib"], after["lib"]);
        assert_ne!(before["app"], after["app"]);
        assert_eq!(before["other"], after["other"]);
        assert_eq!(before["helper"], after["helper"]);

        // A change to a run dependency does not rebuild its dependent.
        tree.recipe(
            "helper.toml",
            "helper",
            "homepage = \"https://x\"",
            &[],
            &[],
        );
        let after_helper = tree.load().unwrap().input_hashes(&all, &context()).unwrap();
        assert_ne!(after["helper"], after_helper["helper"]);
        assert_eq!(after["app"], after_helper["app"]);
    }

    #[test]
    fn architecture_is_part_of_the_hash() {
        let tree = Tree::new();
        tree.recipe("lib.toml", "lib", "", &[], &[]);
        let set = tree.load().unwrap();
        let x86 = set.input_hashes(&["lib"], &context()).unwrap();
        let arm = set
            .input_hashes(
                &["lib"],
                &HashContext {
                    arch: Arch::Aarch64,
                    host_id: None,
                },
            )
            .unwrap();
        assert_ne!(x86["lib"], arm["lib"]);
    }

    #[test]
    fn host_recipes_need_and_hash_the_builder_id_target_recipes_ignore_it() {
        let tree = Tree::new();
        tree.recipe("s0.toml", "stage0-binutils", "stage = 0", &[], &[]);
        let path = tree.0.join("s0.toml");
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("[build]", "[build]\nenvironment = \"host\"");
        fs::write(&path, text).unwrap();
        tree.recipe("lib.toml", "lib", "", &[], &[]);
        let set = tree.load().unwrap();

        assert!(matches!(
            set.input_hashes(&["stage0-binutils"], &context()),
            Err(Error::MissingHostId(_))
        ));

        let with = |id: &str| HashContext {
            arch: Arch::X86_64,
            host_id: Some(id.to_owned()),
        };
        let all = ["stage0-binutils", "lib"];
        let one = set.input_hashes(&all, &with("sha256:aaa")).unwrap();
        let two = set.input_hashes(&all, &with("sha256:bbb")).unwrap();
        assert_ne!(one["stage0-binutils"], two["stage0-binutils"]);
        assert_eq!(one["lib"], two["lib"]);
    }

    #[test]
    fn explain_shows_what_was_hashed() {
        let tree = Tree::new();
        tree.recipe("app.toml", "app", "", &["lib"], &[]);
        tree.recipe("lib.toml", "lib", "", &[], &[]);
        let set = tree.load().unwrap();
        let text = set.explain_hash("app", &context()).unwrap();
        assert!(text.starts_with("hideforge-input-v1\nname app\nrecipe "));
        assert!(text.contains("\narch x86_64\n"));
        let lib = set.input_hashes(&["lib"], &context()).unwrap()["lib"];
        assert!(text.contains(&format!("\ninput lib {lib}\n")));
    }
}
