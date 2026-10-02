# Contributing

hideOS is pre-alpha. The design is in [ARCHITECTURE.md](ARCHITECTURE.md);
read it before proposing changes to it.

## Setup

**Rust** via [rustup](https://rustup.rs) or Homebrew. MSRV is stable minus two
releases, declared in `Cargo.toml` and checked by CI.

**QEMU** with UEFI firmware for both architectures:

```bash
# macOS — Homebrew's qemu ships edk2 firmware for both
brew install qemu

# Debian/Ubuntu
sudo apt install qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64

# Fedora
sudo dnf install qemu-system-x86 qemu-system-aarch64 edk2-ovmf edk2-aarch64

# Arch
sudo pacman -S qemu-system-x86 qemu-system-aarch64 edk2-ovmf edk2-aarch64
```

**Docker or podman**, for the builder. On macOS, Docker Desktop.

Then:

```bash
cargo xtask doctor
```

It lists what is there and what is missing, and exits non-zero until
everything is.

## The builder

Building hideOS needs Linux. `cargo xtask builder` runs a Debian container
with every tool the current milestones need, defined in
[tools/builder/Containerfile](tools/builder/Containerfile):

```bash
cargo xtask builder build                # once, and after the Containerfile changes
cargo xtask builder shell                # interactive
cargo xtask builder run -- cargo test    # one command
```

The checkout is mounted at `/src`. **Builds happen in `/work`**, a named
volume on the container runtime's own Linux filesystem, and `CARGO_TARGET_DIR`
points there. That is not only for speed: macOS filesystems are usually
case-insensitive, and the Linux source tree contains files whose names differ
only in case. A kernel built in a macOS checkout is a kernel missing files.
`doctor` tells you which kind of filesystem your checkout is on.

## Booting

```bash
cargo xtask firmware-smoke --arch x86_64
cargo xtask firmware-smoke --arch aarch64
```

Boots the UEFI firmware in QEMU with no disk and checks that it reaches boot
device selection. Every later boot test stands on this one. The host's own
architecture is accelerated (KVM on Linux, HVF on macOS); the other runs on
QEMU's JIT and takes longer.

## Working over SSH on macOS

The macOS keychain is locked in SSH sessions, including a remote Claude Code
session. `gh` keeps its token there, so it reports the token as invalid even
though it is not. Unlock it once per session, from inside that session:

```bash
security unlock-keychain ~/Library/Keychains/login.keychain-db
```
