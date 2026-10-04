# hideforge

hideOS's build system: TOML recipes in, a sealed system image out, every
package compiled from upstream source in a sandbox with no network. This
page is a map; the specifications are
[docs/HIDEFORGE.md](../HIDEFORGE.md) (the model) and
[docs/RECIPE_FORMAT.md](../RECIPE_FORMAT.md) (the file format), and they
decide. How to run it is on [[Building]].

## The model

A **recipe** says how to turn sources into files. Building it produces an
**output**: a directory tree laid out as it will be under `/`, stored once
at `/work/store/<input-hash>-<name>-<version>/` and never changed after.

The **input hash** covers everything that can change the output: the recipe
file and its patches byte for byte, every source's SHA-256, the input
hashes of every build dependency, the architecture, the builder image for
stage-0 builds, and `OUTPUT_POLICY` — the version of what hideforge itself
does to an output after the build. Same inputs, same path: a store path
that exists is a build that does not run again. A failed build leaves no
store path behind.

`cargo xtask forge -- hash NAME --explain` prints a hash and what went into
it.

## Recipes

zlib's, as it is in [recipes/system/toolchain/zlib.toml](../../recipes/system/toolchain/zlib.toml):

```toml
[package]
name = "zlib"
version = "1.3.2"
description = "Compression library"
license = "Zlib"
homepage = "https://zlib.net"

# TLS only: the release signature is not checked yet.
[[source]]
url = "https://zlib.net/zlib-1.3.2.tar.xz"
sha256 = "d7a0654783a4da529d1bb793b7ad9c3318020af77667bcae35f95d0e42a792f3"

[depends]
build = ["stage1-root", "glibc"]
run = ["glibc"]

[build]
script = """
./configure --prefix=/usr
make -j"$JOBS"
make install
rm -f /usr/lib/libz.a
"""
```

- Any `.toml` under [recipes/](../../recipes); the directories are for
  people (`stage0`, `stage1`, and `system/` by area: `base`, `boot`,
  `desktop`, `cosmic`, `apps`, `images`…). A recipe is known by
  `package.name`, unique across the tree. There is one version of each
  recipe at any commit, so dependencies are names only.
- Files a recipe uses — patches, configuration, units — live in a directory
  next to it with its file stem, and the script finds them in `$FILES`.
- **Sources** are `https://` only, each with its SHA-256, written down from
  a download checked against the upstream signature or published checksum
  where there is one; the commit adding or bumping a source says which. A
  GNU URL is also tried on two mirrors; the digest makes any copy as good
  as the original.
- **Patches** (`build.patches`) are applied with `patch -p1` after
  unpacking. Each carries a header saying why it exists and whether it was
  sent upstream.
- **Vendoring** (`build.vendor = "cargo"`): a build that would download
  crates cannot, so hideforge downloads every crates.io package in the
  source's `Cargo.lock` before the sandbox exists, checks each against the
  lockfile's SHA-256, and points cargo at that copy, offline. Git
  dependencies are refused: nothing pins their content.
- **This repository** (`build.workspace = true`): hidestage, `hide` and the
  other crates here are built from the Cargo workspace as git tracks it,
  archived deterministically; changing a crate rebuilds exactly the recipes
  that use the workspace.
- **Images** are recipes too: [images/minimal.toml](../../recipes/system/images/minimal.toml)
  and [images/workstation.toml](../../recipes/system/images/workstation.toml),
  whose run closure is the system, with `[image] exclude` for what an image
  leaves out.

## The sandbox

Every build runs in fresh Linux namespaces — mount, PID, network (loopback
only, and down), UTS (hostname `hideforge`), IPC — inside the privileged
builder container
([crates/hideforge/src/sandbox.rs](../../crates/hideforge/src/sandbox.rs)).
The namespaces are for hermeticity, what a build can see; the container is
the boundary with the host.

The build installs into the live root it runs in, and its output is
exactly what it changed: the root is an overlay whose lower layers are the
outputs of its dependencies and whose empty upper layer becomes the store
path. No `DESTDIR`, and no package that forgets to honour it. Three rules
keep that honest, each checked after the build:

- a build may not delete or replace a file its own stage provided;
- two outputs of the same stage may not provide the same path;
- a stage-2 output may only link stage-2 libraries: every `DT_NEEDED` of
  every ELF file must be found in stage 2.

Indexes over all packages' files (`share/info/dir`, `etc/ld.so.cache`) are
removed from every output; the image regenerates what it needs.
`cargo xtask forge-selftest` checks these guarantees; see [[Testing]].

## The bootstrap

Three stages, after Linux From Scratch, with the target triple
`<arch>-hideos-linux-gnu` so that a host tool is never mistaken for a
target one:

| Stage | Runs in | Builds | 2018 laptop |
|---|---|---|---|
| 0 | the builder (`host`) | a cross toolchain, then temporary tools and a native GCC | about 42 min |
| 1 | stage 0's tree | the rest of the temporary tools: gettext, bison, Perl, Python, Texinfo | about 13 min |
| 2 | the stages below | the system, final toolchain first | about an hour for Minimal |

Only stage 2 goes into an image. Rust cannot be bootstrapped from source:
`rustc` starts from a published binary pinned by SHA-256 like any source,
and the compiler that ships is built from source by it.

## Reproducibility

`SOURCE_DATE_EPOCH` is the newest time in the source archive, and every
file in an output is clamped to it; the build path is always `/build/src`;
locale `C`, timezone UTC. A check that builds a recipe twice and compares
the outputs bit for bit is planned, not written.

## From outputs to an image

`hideforge image NAME` merges the run closure of an image recipe — stage-2
outputs only — strips debug information from every ELF file (the store
keeps it), refuses the image if any `DT_NEEDED` library is missing, and
writes the composefs payload, the OCI image and the signed UKI; see
[[Building]] for the files and [[Updates and rollback]] for the OCI image.
