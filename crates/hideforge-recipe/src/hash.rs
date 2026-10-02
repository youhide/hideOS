//! Input hashes: the name of a build, derived from everything that can change
//! what it produces.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

/// An architecture hideOS is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    /// `<arch>-hideos-linux-gnu`. Deliberately not the builder's
    /// `<arch>-linux-gnu`, so a host tool can never satisfy a target lookup.
    pub fn target_triple(self) -> String {
        format!("{}-hideos-linux-gnu", self.as_str())
    }
}

impl FromStr for Arch {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x86_64" => Ok(Arch::X86_64),
            "aarch64" => Ok(Arch::Aarch64),
            other => Err(format!(
                "unknown architecture `{other}`; known: x86_64, aarch64"
            )),
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is the same for every recipe in one build, and still part of what
/// each one produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashContext {
    pub arch: Arch,
    /// The builder image ID. Needed only by `host` recipes, which see the
    /// builder's compilers; a `target` recipe's hash must not change when the
    /// builder does, or a new Debian point release would rebuild the world.
    pub host_id: Option<String>,
    /// SHA-256 of this repository's workspace snapshot, for recipes that
    /// build from it. See `Build::workspace`.
    pub workspace: Option<String>,
}

/// SHA-256 over a recipe's inputs. See `docs/HIDEFORGE.md`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputHash([u8; 32]);

impl InputHash {
    /// The prefix used in store paths: 128 bits, 32 hex digits. Enough that
    /// a collision is not a thing to plan for, short enough to read in a log.
    pub fn short(&self) -> String {
        hex(self.0.get(..16).unwrap_or(&self.0))
    }
}

impl fmt::Display for InputHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for InputHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InputHash({self})")
    }
}

/// The version of what hideforge does to an output after a build: which
/// image indexes it removes, what it checks, how it clamps timestamps. A
/// change to any of that changes what an output contains without changing any
/// recipe, so it must change every hash.
///
/// Bump it in the same commit as such a change. Everything rebuilds, which
/// is the point. Version 1 hashed nothing, to keep the store that existed
/// when this was introduced; from 2 on it is a line in every hash.
pub const OUTPUT_POLICY: u32 = 1;

/// Builds the text that is hashed, one line per input, so that what went
/// into a hash can be printed and diffed when two hashes unexpectedly differ.
pub(crate) struct Hasher {
    lines: String,
}

impl Hasher {
    /// The version line. Bump it when the hashing scheme changes, and every
    /// store path changes with it rather than silently colliding with paths
    /// made under the old scheme.
    pub(crate) fn new() -> Hasher {
        let mut lines = "hideforge-input-v1\n".to_owned();
        if OUTPUT_POLICY > 1 {
            lines.push_str(&format!("policy {OUTPUT_POLICY}\n"));
        }
        Hasher { lines }
    }

    pub(crate) fn line(&mut self, key: &str, value: &str) {
        self.lines.push_str(key);
        self.lines.push(' ');
        self.lines.push_str(value);
        self.lines.push('\n');
    }

    pub(crate) fn finish(self) -> (InputHash, String) {
        let digest: [u8; 32] = Sha256::digest(self.lines.as_bytes()).into();
        (InputHash(digest), self.lines)
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
