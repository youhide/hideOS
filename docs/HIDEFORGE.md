# hideforge

How hideOS is built from source. The recipe file format is specified
separately in [RECIPE_FORMAT.md](RECIPE_FORMAT.md).

## The model

A **recipe** says how to turn sources into files. Building it produces an
**output**: a directory tree, laid out as it will appear relative to `/` on a
hideOS machine, stored once under the **store**.

```
/work/store/<input-hash>-<name>-<version>/
```

The **input hash** covers everything that can change the output:

- the recipe file, byte for byte, and every patch it names;
- the SHA-256 of every source archive;
- the input hashes of every build dependency, recursively;
- the target architecture;
- for host-environment builds, the builder image ID.

Same inputs, same path, so a store path that already exists is a build that
does not need to run. Change one byte of one recipe and everything that
depends on it gets a new path and rebuilds; nothing else does.

Outputs are never modified after the build that made them. An image is
assembled by merging outputs, never by building into a shared tree.

## The sandbox

Every build runs in a fresh set of Linux namespaces:

| Namespace | Why                                                                 |
|-----------|---------------------------------------------------------------------|
| mount     | The build sees only its declared inputs                             |
| network   | Only a loopback interface, and it is down: **no network, ever**     |
| PID       | The build cannot see or signal anything outside it                  |
| UTS       | Hostname is `hideforge`, so it cannot leak into an output           |
| IPC       | No shared memory with anything outside                              |

Sources are fetched *before* the sandbox exists, by hideforge, verified
against the recipe's SHA-256, and unpacked into the build directory. Anything
the build needs from the network it does not get, and the failure is the
recipe's to fix: a build that works only online is not reproducible.

### Capturing the output

The build installs **into the live root it runs in**, and its output is
exactly what it changed. The root is an overlay:

- **lower layers**: the outputs of every build dependency, stacked read-only;
- **upper layer**: an empty directory, which becomes the output.

`make install` writes `/usr/bin/foo`; the write lands in the upper layer;
the upper layer is the store path. No `DESTDIR`, no staging directory, no
packages that forget to honour either. Scratch space — `/build`, `/tmp`,
`$HOME` — is mounted separately, so nothing written there is captured.

Two rules keep this honest:

- **A build may not delete or replace a file it inherited.** Overlay records
  that as a whiteout or a copied-up file in the upper layer. hideforge scans
  the output after the build and fails it if it finds either, naming the path.
  A build that rewrites a dependency's file (an index, a cache, a shared
  `info/dir`) has to stop doing that.
- **Two outputs may not provide the same path** in one sandbox or one image.
  The merge fails and names both.

### Two environments

| Environment  | `/` is                                        | Used by                    |
|--------------|-----------------------------------------------|----------------------------|
| `host`       | The builder container, read-only; the overlay is mounted at `/sysroot` | Bootstrap stage 0 only |
| `target`     | The overlay itself                            | Everything else            |

`host` builds can run the builder's compilers and are therefore not hermetic
in the strict sense: their input hash includes the builder image ID, so a new
builder image rebuilds them, but two builder images that differ produce
different outputs. That is accepted for stage 0 alone, whose whole purpose is
to produce a toolchain that no longer needs the host. From stage 1 on,
nothing from the builder is visible.

## The bootstrap

Three stages, after Linux From Scratch. The target triple is
`<arch>-hideos-linux-gnu` throughout, distinct from the builder's
`<arch>-linux-gnu`, so a host tool can never be mistaken for a target one.

| Stage | Environment | Builds                                                                   |
|-------|-------------|--------------------------------------------------------------------------|
| 0     | `host`      | A cross toolchain in `/tools` (binutils, GCC, Linux headers, glibc), then a minimal set of temporary tools cross-compiled into `/sysroot` |
| 1     | `target`    | The final toolchain, natively, inside the stage-0 tree                   |
| 2     | `target`    | Everything in the image, with the stage-1 toolchain and nothing older    |

Stage 0's outputs exist only to build stage 1. Stage 1's outputs exist only
to build stage 2. An image contains stage-2 outputs and nothing else, and
`hideforge` refuses to put anything from an earlier stage into one.

**What is not bootstrapped from source.** Rust: `rustc` is written in Rust
and has to start from a published binary. The recipe pins it by SHA-256 like
any source, and the compiler that ships is built from source by it. The same
holds for any other self-hosting compiler.

## Reproducibility

- `SOURCE_DATE_EPOCH` is set to the newest modification time in the source
  archive, and every file in an output is clamped to it.
- Build paths are fixed (`/build/src`), so they are the same in every
  sandbox on every machine.
- Locale `C` (always present, unlike `C.UTF-8`, which must be installed),
  timezone UTC, hostname `hideforge`.
- `cargo xtask` gains a check that builds a recipe twice in different
  sandboxes and compares the outputs bit for bit.

## Layout on disk

Everything lives in the builder's `/work` volume:

```
/work/sources/<sha256>          downloaded archives, by content
/work/store/<hash>-<name>-<v>/  outputs
/work/build/<hash>/             scratch for a build in progress; deleted after
/work/logs/<hash>.log           every build's full output, kept
```

A failed build leaves no store path behind: the output is written to a
temporary name and renamed into the store only on success, so a store path
that exists is a build that finished.
