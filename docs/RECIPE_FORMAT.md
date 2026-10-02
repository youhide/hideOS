# Recipe format

A recipe is a TOML file under `recipes/`. How hideforge runs it is in
[HIDEFORGE.md](HIDEFORGE.md).

```toml
[package]
name = "zlib"
version = "1.3.1"
description = "Compression library"
license = "Zlib"
homepage = "https://zlib.net"

[[source]]
url = "https://zlib.net/zlib-1.3.1.tar.xz"
sha256 = "38ef96b8dfe510d42707d9c781877914792541133e1870841463bfa73f883e32"

[depends]
build = ["stage1-toolchain"]
run = []

[build]
script = """
./configure --prefix=/usr
make -j"$JOBS"
make install
"""
```

## Where recipes live

Any `.toml` file anywhere under `recipes/`. The directory structure is for
people; hideforge reads recursively and identifies recipes only by
`package.name`, which must be unique across the tree.

Files a recipe refers to — patches, configuration — live in a directory next
to it with the recipe's file stem: `recipes/base/zlib.toml` finds its patches
in `recipes/base/zlib/`. Every file there is part of the recipe's input
hash, and the script finds them all in `$FILES`. That directory is not
searched for recipes, so it can hold `.toml` files that are data.

## `[package]`

| Key           | Required | Meaning                                                    |
|---------------|----------|------------------------------------------------------------|
| `name`        | yes      | Lowercase ASCII letters, digits and `-`; starts with a letter. At most 64 bytes. |
| `version`     | yes      | Upstream's version, as upstream writes it. No spaces or `/`. |
| `description` | yes      | One line.                                                  |
| `license`     | yes      | An SPDX expression.                                        |
| `homepage`    | no       | URL.                                                       |
| `stage`       | no       | `0`, `1` or `2`. Default `2`. See [Stages](#stages).       |

## `[[source]]`

Zero or more. Each is downloaded, checked, and unpacked before the build
starts.

| Key       | Required | Meaning                                                          |
|-----------|----------|------------------------------------------------------------------|
| `url`     | yes      | `https://` only.                                                 |
| `sha256`  | yes      | 64 lowercase hex digits. A mismatch is a hard failure.           |
| `dest`    | no       | Where to unpack, relative to `/build/src`. Default: `.`          |
| `strip`   | no       | Leading path components to strip. Default `1`, which is right for a tarball with a single top directory. |
| `extract` | no       | `false` to copy the file into `dest` unopened. Default `true`.   |
| `arch`    | no       | `x86_64` or `aarch64`: used only when building for that architecture. For sources that are themselves built for one, like a binary toolchain. |

The first source unpacks into `/build/src` itself, which is where the script
starts. Additional sources typically go into a subdirectory — GCC's bundled
`gmp`, `mpfr` and `mpc`, for example:

```toml
[[source]]
url = "https://ftp.gnu.org/gnu/gmp/gmp-6.3.0.tar.xz"
sha256 = "..."
dest = "gmp"
```

**Mirrors.** A `https://ftp.gnu.org/gnu/` URL is also tried at
`ftpmirror.gnu.org` and `mirrors.kernel.org` when the original fails. Write the
canonical URL; the digest makes any mirror's copy as good as the original.

**Where a hash comes from.** A recipe's `sha256` is written down when the
recipe is, from a download checked against the upstream signature or
published checksum where one exists. The commit adding or bumping a source
says which.

## `build.patches`

```toml
[build]
patches = ["fix-musl-only-assumption.patch"]
```

A key in `[build]`, not at the top level: TOML puts a top-level key written
after a table header inside that table, so its meaning would depend on where
in the file it was written. Files in the recipe's directory, applied in order with
`patch -p1` in `/build/src` after unpacking. Every patch carries a header
saying why it exists and whether it was sent upstream.

## `build.vendor`

```toml
[build]
vendor = "cargo"
```

For sources whose build would download dependencies, which a sandbox with no
network cannot. hideforge does it instead, before the sandbox exists, and
holds every download to a checksum the source itself pins:

- `"cargo"`: every crates.io package in the `Cargo.lock` at the top of the
  source is downloaded, checked against the lockfile's SHA-256, unpacked into
  `/build/src/.hideforge-vendor`, and cargo is configured to use only that,
  offline. The lockfile is inside the source archive, so the recipe's own
  SHA-256 covers it. Git and other-registry dependencies are refused: they
  come with nothing to check them against.

## `[depends]`

| Key     | Meaning                                                                          |
|---------|----------------------------------------------------------------------------------|
| `build` | Recipes whose outputs are layered into the sandbox root for this build. Only these, and what *they* declare in `run`, are visible. |
| `run`   | Recipes this one needs at run time. Pulled into an image alongside it, and into any sandbox that has this recipe as a build dependency. |

Names only, no version constraints: the recipe tree is the version
constraint. There is exactly one of each recipe at any commit.

A cycle is an error, reported with its path.

## `[build]`

| Key           | Required | Meaning                                                     |
|---------------|----------|-------------------------------------------------------------|
| `script`      | yes      | Run by `bash -euo pipefail` in `/build/src`.                |
| `environment` | no       | `host` or `target`. Default `target`. See [HIDEFORGE.md](HIDEFORGE.md#two-environments). Only stage-0 recipes may use `host`. |

The script installs into `/`, or into `/sysroot` in a `host` build, and
everything it writes there becomes the output. See
[Capturing the output](HIDEFORGE.md#capturing-the-output).

Environment variables the script can rely on:

| Variable            | Value                                                 |
|---------------------|-------------------------------------------------------|
| `JOBS`              | Parallel jobs to use with `make -j`                    |
| `ARCH`              | `x86_64` or `aarch64`                                 |
| `TARGET`            | `$ARCH-hideos-linux-gnu`                              |
| `SYSROOT`           | `/sysroot` in a `host` build, `/` in a `target` build |
| `SOURCE_DATE_EPOCH` | See [Reproducibility](HIDEFORGE.md#reproducibility)    |
| `FILES`             | The recipe's own files, from the directory next to it  |
| `HOME`              | A scratch directory, not captured                     |
| `PATH`              | `/tools/bin` or `$SYSROOT/tools/bin` first if present, then the standard directories |

Nothing else from the caller's environment is passed through.

## `[image]`

Only for a recipe that is assembled into an image with `hideforge image`.

| Key       | Meaning                                                              |
|-----------|----------------------------------------------------------------------|
| `exclude` | Paths left out of the image: `dir/` for a directory and everything in it, `*.ext` for every file with that extension, anything else for one exact path. Relative to `/`. |

Whatever is excluded, the image must still work: hideforge checks that every
shared library any ELF file in the image names in `DT_NEEDED` is in the
image, and refuses the image if one is not. Debug information is stripped
from every ELF file in an image; the store keeps it.

## Stages

`stage` places a recipe in the bootstrap described in
[HIDEFORGE.md](HIDEFORGE.md#the-bootstrap). hideforge enforces the ordering:

- a stage-*n* recipe may depend on recipes of stage *n* and *n−1* only (and
  sees, through their run dependencies, whatever those were built on);
- only stage 0 may use `environment = "host"`;
- only stage 2 may appear in an image.

Stage-0 and stage-1 names start with `stage0-` and `stage1-`, so the stage is
visible wherever the name is.
