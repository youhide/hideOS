# Building

hideOS builds on Linux. On macOS, and anywhere else, it builds in a
container, the **builder**, which `cargo xtask` drives from the host. The
setup is in [CONTRIBUTING.md](../../CONTRIBUTING.md); the build system
itself is on the [[hideforge]] page.

## Before anything

```sh
cargo xtask doctor
```

Lists the tools and UEFI firmware this machine has and lacks — QEMU for
both architectures, docker or podman, the firmware files — and exits
non-zero until everything is there. It also says whether the checkout is
on a case-insensitive filesystem, which matters below.

## The builder

```sh
cargo xtask builder build                # once, and after the Containerfile changes
cargo xtask builder shell                # a shell inside it
cargo xtask builder run -- cargo test    # one command inside it
```

A Debian container, defined in
[tools/builder/Containerfile](../../tools/builder/Containerfile), tagged
`hideos-builder:dev`. docker is used if present, then podman;
`HIDEOS_CONTAINER` picks one. On the Intel Mac where H0 was verified it
built in 78 seconds.

What it mounts:

| In the builder | What it is |
|---|---|
| `/src` | The checkout, read and written as it is. |
| `/work` | The named volume `hideos-work`: hideforge's store, sources, logs, and cargo's target directory. |
| `/usr/local/cargo/registry` | The named volume `hideos-cargo-registry`, crates kept across runs. |

Builds happen in `/work`, never in the checkout. macOS filesystems are
usually case-insensitive and the Linux source tree has files whose names
differ only in case: a kernel built in a macOS checkout is a kernel
missing files. The volume lives on the container runtime's own Linux
filesystem, which is case-sensitive and faster.

## An image

```sh
cargo xtask image                          # hideOS Minimal
cargo xtask image --edition workstation    # hideOS Workstation
cargo xtask image --arch aarch64           # not built yet; see ROADMAP H1
```

This runs hideforge, built in release mode, inside the builder, and asks it
for the edition's image with everything a disk needs: the composefs
payload, the OCI image, the UKI signed with the development key (see
[[Boot chain]]), and hideBoot as the boot manager. The image version is
`--image-version N`, by default the number of commits on `HEAD`, so two
builds of the same commit are the same version.

Never run two image builds at once: see [[Development notes#one-image-build-at-a-time]].

### Editions

| Edition | What it is | Boots |
|---|---|---|
| `minimal` | Kernel, oxinit, zsh, the core utilities, networking. | From RAM (`cargo xtask boot`) or from its disk. |
| `workstation` | Minimal plus COSMIC, PipeWire, polkit, Flatpak. | From its disk only, in a window. |

Workstation's image recipe depends on `minimal-base`, so both ship the same
build of every shared package. See
[ARCHITECTURE.md, "Editions"](../../ARCHITECTURE.md#editions).

### How long

The first `image` bootstraps a toolchain and builds the whole system from
source: about two hours on a 2018 laptop — stage 0 about 42 minutes,
stage 1 about 13, stage 2 about an hour. The first Workstation build is
most of a day on the same laptop: LLVM, Mesa and about twenty COSMIC
components. After that only what changed rebuilds, because every output is
stored under the hash of its inputs.

### Where the outputs go

`target/images/EDITION-ARCH/` in the checkout, so that QEMU on the host can
read them:

| File | What it is |
|---|---|
| `payload.tar` | What `hide install` installs: the composefs repository, the factory `/etc`, the ESP. |
| `image.oci.tar` | The same system as an OCI archive: what `hide update` takes. |
| `image.digest` | The fs-verity digest of the boot image, the one the UKI carries. |
| `hideos-EDITION-VERSION-DIGEST.efi` | The signed UKI. |
| `bootmanager.efi` | hideBoot, signed. |
| `vmlinuz`, `initramfs.cpio` | Minimal only: the kernel and the whole root, to boot from RAM. |
| `installer.efi`, `recovery.efi` | Minimal only: the installer's and the recovery system's UKIs. |

hideforge's own store, logs and sources stay in the `/work` volume; see
[docs/HIDEFORGE.md](../HIDEFORGE.md#layout-on-disk).

Every UKI `cargo xtask` builds is a test image: it appends
`console=ttyS0` to the command line so that the tests can read and type on
the serial port. See [[Development notes#the-screen-is-the-console-tests-append-the-serial-port]].

## A disk, and booting it

```sh
cargo xtask install                  # target/images/minimal-x86_64/disk.raw
cargo xtask boot                     # Minimal from RAM; quit QEMU with Ctrl-A X
cargo xtask boot --disk              # the disk, through UEFI and hideBoot
cargo xtask boot --disk --secure-boot
cargo xtask install --edition workstation
cargo xtask boot --edition workstation
```

`install` does not assemble a disk image in the builder: it boots Minimal
from RAM in QEMU with `hide install` as PID 1, the payload on one virtual
disk and an empty 16 GiB sparse file on the other. The builder's kernel may
not have fs-verity, and a hideOS kernel does; and every disk image is then
also a test of the installer's code. See
[ARCHITECTURE.md, "Disk images are installed, not assembled"](../../ARCHITECTURE.md#disk-images-are-installed-not-assembled).

The development disk's account is `hide`, password `hide`.

Each image directory keeps the firmware's variables in `efivars.fd` (and
`efivars-secure.fd` for Secure Boot), as a machine keeps its NVRAM; a new
disk starts with fresh ones.

## An installer medium

```sh
cargo xtask installer --edition workstation
```

A disk image for a USB stick, `target/images/installer-EDITION-ARCH.img`:
an ESP with hideBoot, the installer's UKI and the recovery system, and a
partition holding the edition's payload. See [[Install and recover]].

## hideforge directly

```sh
cargo xtask forge -- list                 # every recipe
cargo xtask forge -- order minimal        # what building it builds, in order
cargo xtask forge -- build linux          # one recipe and what it needs
cargo xtask forge -- hash linux --explain # its input hash, and what went into it
```

`forge` passes its arguments to hideforge in the builder. The commands are
listed at the top of
[crates/hideforge/src/main.rs](../../crates/hideforge/src/main.rs).
