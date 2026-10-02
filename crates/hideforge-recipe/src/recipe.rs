//! One recipe file: parsing and validation.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Error;

/// A recipe that parsed and passed every check that needs only its own file.
/// Checks that need the other recipes — dependencies exist, stages are in
/// order, no cycles — are [`RecipeSet`](crate::RecipeSet)'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub package: Package,
    pub sources: Vec<Source>,
    pub depends: Depends,
    pub build: Build,
    pub image: Image,
}

/// What `hideforge image` does with this recipe's run closure, when it is
/// the image being assembled. Ignored otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    /// Paths left out of the image. `dir/` is a directory and everything in
    /// it; `*.ext` is every file with that extension; anything else is one
    /// exact path. All relative to `/`.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Image {
    /// Whether `path`, relative to `/`, is left out.
    pub fn excludes(&self, path: &str) -> bool {
        self.exclude.iter().any(|pattern| {
            if let Some(dir) = pattern.strip_suffix('/') {
                path == dir || path.starts_with(pattern.as_str())
            } else if let Some(ext) = pattern.strip_prefix('*') {
                path.ends_with(ext)
            } else {
                path == pattern
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub license: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub stage: Stage,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub url: String,
    pub sha256: String,
    #[serde(default = "Source::default_dest")]
    pub dest: String,
    #[serde(default = "Source::default_strip")]
    pub strip: u32,
    #[serde(default = "Source::default_extract")]
    pub extract: bool,
    /// Only for this architecture. For sources that are themselves built for
    /// one, like a binary toolchain.
    #[serde(default)]
    pub arch: Option<String>,
}

impl Source {
    fn default_dest() -> String {
        ".".to_owned()
    }
    fn default_strip() -> u32 {
        1
    }
    fn default_extract() -> bool {
        true
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Depends {
    #[serde(default)]
    pub build: Vec<String>,
    #[serde(default)]
    pub run: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub script: String,
    #[serde(default)]
    pub environment: Environment,
    /// Patch file names, in the directory next to the recipe named after its
    /// file stem. Here rather than at the top level of the file because TOML
    /// puts a top-level key written after `[package]` inside `[package]`, and
    /// a format whose meaning depends on key order is a trap.
    #[serde(default)]
    pub patches: Vec<String>,
    /// Dependencies a language package manager would download, fetched by
    /// hideforge before the sandbox exists instead. See `RECIPE_FORMAT.md`.
    #[serde(default)]
    pub vendor: Option<Vendor>,
    /// Build from this repository's own Cargo workspace — `Cargo.toml`,
    /// `Cargo.lock` and `crates/`, as git tracks them — unpacked into
    /// `/build/src` before any `[[source]]`. For hideOS's own programs:
    /// hidestage, hide.
    #[serde(default)]
    pub workspace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    /// Every crates.io package in the source's `Cargo.lock`, checked against
    /// the lockfile's own SHA-256.
    Cargo,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Zero,
    One,
    #[default]
    Two,
}

impl Stage {
    pub fn number(self) -> u8 {
        match self {
            Stage::Zero => 0,
            Stage::One => 1,
            Stage::Two => 2,
        }
    }

    /// The name prefix a recipe of this stage must carry, if any.
    fn prefix(self) -> Option<&'static str> {
        match self {
            Stage::Zero => Some("stage0-"),
            Stage::One => Some("stage1-"),
            Stage::Two => None,
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

impl<'de> Deserialize<'de> for Stage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            0 => Ok(Stage::Zero),
            1 => Ok(Stage::One),
            2 => Ok(Stage::Two),
            other => Err(serde::de::Error::custom(format!(
                "stage must be 0, 1 or 2, not {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// The builder container is `/`, the overlay is at `/sysroot`. Stage 0
    /// only.
    Host,
    /// The overlay is `/`. Nothing from the builder is visible.
    #[default]
    Target,
}

/// The file as written, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    package: Package,
    #[serde(default)]
    source: Vec<Source>,
    #[serde(default)]
    depends: Depends,
    build: Build,
    #[serde(default)]
    image: Image,
}

impl Source {
    /// Whether this source is used when building for `arch`.
    pub fn applies_to(&self, arch: crate::Arch) -> bool {
        self.arch
            .as_deref()
            .is_none_or(|wanted| wanted == arch.as_str())
    }
}

impl Recipe {
    /// Parses and validates one recipe. `path` is used only in errors.
    pub fn parse(text: &str, path: &Path) -> Result<Recipe, Error> {
        let raw: RawRecipe = basic_toml::from_str(text).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let recipe = Recipe {
            package: raw.package,
            sources: raw.source,
            depends: raw.depends,
            build: raw.build,
            image: raw.image,
        };
        recipe.validate(path)?;
        Ok(recipe)
    }

    pub fn name(&self) -> &str {
        &self.package.name
    }

    pub fn stage(&self) -> Stage {
        self.package.stage
    }

    /// Build dependencies, then run dependencies, each in file order.
    pub fn all_dependencies(&self) -> impl Iterator<Item = &str> {
        self.depends
            .build
            .iter()
            .chain(self.depends.run.iter())
            .map(String::as_str)
    }

    fn validate(&self, path: &Path) -> Result<(), Error> {
        let invalid = |field: &'static str, reason: String| Error::Invalid {
            path: path.to_path_buf(),
            field,
            reason,
        };
        let package = &self.package;

        check_name(&package.name).map_err(|reason| invalid("package.name", reason))?;
        match package.stage.prefix() {
            Some(prefix) if !package.name.starts_with(prefix) => {
                return Err(invalid(
                    "package.name",
                    format!(
                        "must start with `{prefix}` in a stage {} recipe",
                        package.stage
                    ),
                ));
            }
            None if package.name.starts_with("stage0-") || package.name.starts_with("stage1-") => {
                return Err(invalid(
                    "package.name",
                    "starts with a stage prefix, but the recipe is stage 2".to_owned(),
                ));
            }
            _ => {}
        }

        if package.version.is_empty()
            || package
                .version
                .chars()
                .any(|c| c.is_whitespace() || c == '/')
        {
            return Err(invalid(
                "package.version",
                "must be non-empty, with no whitespace or `/`".to_owned(),
            ));
        }
        if package.description.trim().is_empty() || package.description.contains('\n') {
            return Err(invalid(
                "package.description",
                "must be one non-empty line".to_owned(),
            ));
        }
        if package.license.trim().is_empty() {
            return Err(invalid("package.license", "must not be empty".to_owned()));
        }

        for source in &self.sources {
            if !source.url.starts_with("https://") {
                return Err(invalid(
                    "source.url",
                    format!("must be https://, not `{}`", source.url),
                ));
            }
            if source.sha256.len() != 64
                || !source
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid(
                    "source.sha256",
                    format!("must be 64 lowercase hex digits, not `{}`", source.sha256),
                ));
            }
            check_relative(&source.dest).map_err(|reason| invalid("source.dest", reason))?;
            if let Some(arch) = &source.arch {
                arch.parse::<crate::Arch>()
                    .map_err(|reason| invalid("source.arch", reason))?;
            }
        }

        for patch in &self.build.patches {
            if patch.is_empty() || patch.contains('/') || patch.starts_with('.') {
                return Err(invalid(
                    "build.patches",
                    format!("`{patch}` must be a plain file name in the recipe's directory"),
                ));
            }
        }

        for pattern in &self.image.exclude {
            let body = pattern.trim_start_matches('*');
            if pattern.is_empty()
                || pattern.starts_with('/')
                || pattern.split('/').any(|part| part == "..")
                || (pattern.starts_with('*')
                    && (body.is_empty() || body.contains('*') || body.contains('/')))
            {
                return Err(invalid(
                    "image.exclude",
                    format!("`{pattern}` must be `dir/`, `*.ext` or an exact relative path"),
                ));
            }
        }

        if self.build.script.trim().is_empty() {
            return Err(invalid("build.script", "must not be empty".to_owned()));
        }
        if self.build.environment == Environment::Host && package.stage != Stage::Zero {
            return Err(invalid(
                "build.environment",
                "`host` is for stage 0 only; nothing after the bootstrap may see the builder"
                    .to_owned(),
            ));
        }

        if self.build.environment == Environment::Target && self.depends.build.is_empty() {
            return Err(invalid(
                "depends.build",
                "is empty, so a target build's root is empty too: no shell to run the script \
                 in. Depend on the root of the stage before, or build in `host` (stage 0)"
                    .to_owned(),
            ));
        }

        for dependency in self.all_dependencies() {
            if dependency == package.name {
                return Err(invalid(
                    "depends",
                    "a recipe cannot depend on itself".to_owned(),
                ));
            }
            check_name(dependency).map_err(|reason| invalid("depends", reason))?;
        }

        Ok(())
    }
}

/// Lowercase ASCII letters, digits and `-`, starting with a letter, at most
/// 64 bytes. Names end up in store paths and file names on every filesystem
/// hideOS touches, so nothing that needs quoting anywhere.
fn check_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let starts_with_letter = matches!(bytes.next(), Some(b'a'..=b'z'));
    let rest_ok = bytes.all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'));
    if starts_with_letter && rest_ok && name.len() <= 64 && !name.ends_with('-') {
        Ok(())
    } else {
        Err(format!(
            "`{name}` must be lowercase letters, digits and `-`, start with a letter, \
             not end with `-`, and be at most 64 bytes"
        ))
    }
}

/// A path that stays inside the directory it is relative to.
fn check_relative(path: &str) -> Result<(), String> {
    let as_path = PathBuf::from(path);
    let escapes = as_path.is_absolute()
        || as_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if escapes {
        Err(format!(
            "`{path}` must be relative and must not contain `..`"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZLIB: &str = r#"
[package]
name = "zlib"
version = "1.3.1"
description = "Compression library"
license = "Zlib"

[[source]]
url = "https://zlib.net/zlib-1.3.1.tar.xz"
sha256 = "38ef96b8dfe510d42707d9c781877914792541133e1870841463bfa73f883e32"

[depends]
build = ["stage1-toolchain"]

[build]
script = "./configure --prefix=/usr && make -j$JOBS && make install"
"#;

    fn parse(text: &str) -> Result<Recipe, Error> {
        Recipe::parse(text, Path::new("test.toml"))
    }

    fn field_of(error: Error) -> &'static str {
        match error {
            Error::Invalid { field, .. } => field,
            other => panic!("expected Invalid, got {other}"),
        }
    }

    #[test]
    fn parses_a_complete_recipe_with_defaults() {
        let recipe = parse(ZLIB).unwrap();
        assert_eq!(recipe.name(), "zlib");
        assert_eq!(recipe.stage(), Stage::Two);
        assert_eq!(recipe.build.environment, Environment::Target);
        let source = &recipe.sources[0];
        assert_eq!(source.dest, ".");
        assert_eq!(source.strip, 1);
        assert!(source.extract);
        assert_eq!(recipe.depends.build, vec!["stage1-toolchain"]);
        assert!(recipe.depends.run.is_empty());
    }

    #[test]
    fn unknown_keys_are_errors_not_ignored() {
        let text = ZLIB.replace("license = \"Zlib\"", "license = \"Zlib\"\nlicence = \"x\"");
        assert!(matches!(parse(&text), Err(Error::Parse { .. })));
    }

    #[test]
    fn names_are_checked() {
        for bad in ["Zlib", "1zlib", "z_lib", "zlib-", "z lib", ""] {
            let text = ZLIB.replace("name = \"zlib\"", &format!("name = \"{bad}\""));
            assert_eq!(field_of(parse(&text).unwrap_err()), "package.name", "{bad}");
        }
    }

    #[test]
    fn stage_and_prefix_must_agree() {
        let text = ZLIB.replace("license = \"Zlib\"", "license = \"Zlib\"\nstage = 0");
        assert_eq!(field_of(parse(&text).unwrap_err()), "package.name");

        let text = ZLIB.replace("name = \"zlib\"", "name = \"stage1-zlib\"");
        assert_eq!(field_of(parse(&text).unwrap_err()), "package.name");

        let text = ZLIB
            .replace("name = \"zlib\"", "name = \"stage1-zlib\"")
            .replace("license = \"Zlib\"", "license = \"Zlib\"\nstage = 1");
        assert_eq!(parse(&text).unwrap().stage(), Stage::One);
    }

    #[test]
    fn stage_out_of_range_is_a_parse_error() {
        let text = ZLIB.replace("license = \"Zlib\"", "license = \"Zlib\"\nstage = 3");
        assert!(matches!(parse(&text), Err(Error::Parse { .. })));
    }

    #[test]
    fn sources_must_be_https_with_a_real_digest() {
        let text = ZLIB.replace("https://zlib.net", "http://zlib.net");
        assert_eq!(field_of(parse(&text).unwrap_err()), "source.url");

        let text = ZLIB.replace("38ef96b8", "38EF96B8");
        assert_eq!(field_of(parse(&text).unwrap_err()), "source.sha256");

        let text = ZLIB.replace("38ef96b8", "38ef96b");
        assert_eq!(field_of(parse(&text).unwrap_err()), "source.sha256");
    }

    #[test]
    fn source_dest_cannot_escape_the_build_directory() {
        for bad in ["../up", "/abs", "a/../../b"] {
            let text = ZLIB.replace(
                "sha256 = \"38ef",
                &format!("dest = \"{bad}\"\nsha256 = \"38ef"),
            );
            assert_eq!(field_of(parse(&text).unwrap_err()), "source.dest", "{bad}");
        }
    }

    #[test]
    fn sources_can_be_limited_to_one_architecture() {
        let text = ZLIB.replace("sha256 = \"38ef", "arch = \"aarch64\"\nsha256 = \"38ef");
        let recipe = parse(&text).unwrap();
        assert!(recipe.sources[0].applies_to(crate::Arch::Aarch64));
        assert!(!recipe.sources[0].applies_to(crate::Arch::X86_64));

        let text = ZLIB.replace("sha256 = \"38ef", "arch = \"riscv64\"\nsha256 = \"38ef");
        assert_eq!(field_of(parse(&text).unwrap_err()), "source.arch");
    }

    #[test]
    fn image_exclusions_match_directories_extensions_and_paths() {
        let text =
            format!("{ZLIB}\n[image]\nexclude = [\"usr/include/\", \"*.a\", \"usr/bin/cc\"]\n");
        let image = parse(&text).unwrap().image;
        assert!(image.excludes("usr/include"));
        assert!(image.excludes("usr/include/stdio.h"));
        assert!(!image.excludes("usr/include-other/x.h"));
        assert!(image.excludes("usr/lib/libz.a"));
        assert!(!image.excludes("usr/lib/libz.so"));
        assert!(image.excludes("usr/bin/cc"));
        assert!(!image.excludes("usr/bin/cc1"));

        for bad in ["/usr/include/", "../etc/", "*", "*.a/b", "**/x"] {
            let text = format!("{ZLIB}\n[image]\nexclude = [\"{bad}\"]\n");
            assert_eq!(
                field_of(parse(&text).unwrap_err()),
                "image.exclude",
                "{bad}"
            );
        }
    }

    #[test]
    fn host_environment_is_stage_zero_only() {
        let text = ZLIB.replace("[build]", "[build]\nenvironment = \"host\"");
        assert_eq!(field_of(parse(&text).unwrap_err()), "build.environment");

        let text = ZLIB
            .replace("name = \"zlib\"", "name = \"stage0-zlib\"")
            .replace("license = \"Zlib\"", "license = \"Zlib\"\nstage = 0")
            .replace("stage1-toolchain", "stage0-binutils")
            .replace("[build]", "[build]\nenvironment = \"host\"");
        assert_eq!(parse(&text).unwrap().build.environment, Environment::Host);
    }

    #[test]
    fn patches_are_plain_file_names() {
        for bad in ["../x.patch", "sub/x.patch", ".hidden"] {
            let text = ZLIB.replace("[build]", &format!("[build]\npatches = [\"{bad}\"]"));
            assert_eq!(
                field_of(parse(&text).unwrap_err()),
                "build.patches",
                "{bad}"
            );
        }
    }

    #[test]
    fn a_target_build_needs_something_to_run_in() {
        let text = ZLIB.replace("build = [\"stage1-toolchain\"]", "");
        assert_eq!(field_of(parse(&text).unwrap_err()), "depends.build");
    }

    #[test]
    fn a_recipe_cannot_depend_on_itself() {
        let text = ZLIB.replace("[depends]", "[depends]\nrun = [\"zlib\"]");
        assert_eq!(field_of(parse(&text).unwrap_err()), "depends");
    }
}
