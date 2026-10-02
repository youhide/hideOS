# Roadmap

Milestones are sequential. Each one ends with something that boots and does
something observable in QEMU — and from H4 on, on real hardware — before the
next one starts. There are no dates.

The design these build toward is in [ARCHITECTURE.md](ARCHITECTURE.md).

## H0 — Foundations

**Done.**

- [x] Repository, architecture, roadmap.
- [x] Settle what H1–H3 build on: OCI as the update transport, our own boot
      manager in H7 with `systemd-boot` until then. Secure Boot keys vs. shim
      stays open until the installer, which is the first thing it affects.
- [x] Cargo workspace and `xtask`: `doctor`, `builder`, `firmware-smoke`.
- [x] CI for releases only — a `v*` tag or a manual dispatch — running fmt,
      clippy and tests on Linux and macOS, the MSRV build, advisories, both
      firmware boots, and the builder image.
- [x] A Linux build environment reachable from macOS: the builder container,
      with QEMU and UEFI firmware for both architectures.

Verified on an Intel Mac, over SSH, with Docker Desktop:

- `cargo xtask firmware-smoke` on the host reaches boot device selection in
  1.6s on x86_64 (HVF) and 6.8s on aarch64 (TCG), with Homebrew's edk2.
- The same inside the builder, with Debian's OVMF and AAVMF and no
  acceleration: 9.7s and 8.3s. Two firmware builds, two QEMUs, one check.
- The builder image builds in 78s; its Rust is 1.99.
- `/work` is case-sensitive (`a` and `A` coexist) with 112 GB free, while the
  checkout is not — `doctor` reports exactly that.
- The one CI run, before CI was restricted to releases, passed every job.

Two things the environment taught, both now written down in CONTRIBUTING:

- **The checkout's filesystem cannot hold a Linux build.** macOS volumes are
  case-insensitive and the kernel tree has files differing only in case. So
  builds happen in a named volume on the container runtime's own filesystem,
  never in the checkout.
- **The macOS keychain is locked over SSH.** `gh` reads its token from there,
  and reports a valid token as invalid until the keychain is unlocked inside
  the same session.

## H1 — hideforge builds a system

**In progress.**

The smallest image this project built entirely itself.

- [x] Recipe format specified in `docs/RECIPE_FORMAT.md`, the build model in
      `docs/HIDEFORGE.md`; parsing, validation, dependency graph and input
      hashes in `hideforge-recipe`, tested on any host.
- [x] Hermetic build sandbox: mount, PID, network, UTS and IPC namespaces;
      an overlay whose upper layer is the output; no network; declared
      inputs only; `SOURCE_DATE_EPOCH` from the sources. Checked by
      `cargo xtask forge-selftest`.
- [ ] Bootstrap stages 0 → 1 → 2: cross toolchain, native toolchain, final
      system. Nothing from the host in the output.
- [ ] Recipes: Linux, glibc, GCC/LLVM runtime, Rust, uutils, bash, zsh,
      util-linux, kmod, oxinit.
- [ ] Output: hideOS Minimal, a root tree packed as an initramfs, with the
      kernel next to it.

Done when: QEMU boots the kernel this project compiled, oxinit is PID 1, and a
console shell runs on a glibc userspace — x86_64 and aarch64.

## H2 — The seal

- [ ] composefs image generation in hideforge, deterministic, digest reported.
- [ ] UKI assembly: kernel + initrd + command line with the digest; signing
      with a development key.
- [ ] `hidestage`: mounts, GPT discovery, btrfs, composefs with digest
      verification, bind mounts, `switch_root` into oxinit.
- [ ] A disk image: ESP + btrfs (no LUKS yet), booted from OVMF with
      `systemd-boot` as the stopgap boot manager.

Done when: QEMU boots from a disk into the sealed root; writing to `/usr` fails;
flipping one byte of one object makes reading that file fail with `EIO`; a UKI
with a wrong digest refuses to boot.

## H3 — Updates and rollback

- [ ] OCI image output from hideforge; push to a local registry.
- [ ] `hideupd`: pull, verify signature, unpack into the object store,
      regenerate, compare digest, install UKI atomically with `+3` tries.
- [ ] `hide-boot-ok`: mark the booted deployment good on boot-complete.
- [ ] `hide update`, `hide rollback`, `hide status`.
- [ ] Garbage collection of unreferenced objects.

Done when, in CI: an update from build N to N+1 applies and boots; a power cut
(QEMU killed) at every step of the update leaves N booting; an N+1 that panics,
one that cannot mount, and one that hangs all end, unattended, back on N.

## H4 — Real hardware

- [ ] eudev, linux-firmware, microcode.
- [ ] LUKS2 with TPM2 unlock and a recovery key, in hidestage.
- [ ] NetworkManager + iwd, ntpd-rs, sudo-rs.
- [ ] Suspend and resume, hibernation to the swap file.
- [ ] Hardware watchdog fed by oxinit (oxinit-side work).

Done when: one AMD or Intel laptop boots hideOS from its own disk, unlocks with
TPM2, joins Wi-Fi, suspends and resumes.

## H5 — Desktop

- [ ] dbus-daemon, elogind, polkit.
- [ ] Mesa, PipeWire, WirePlumber.
- [ ] Per-user services in oxinit (oxinit-side work).
- [ ] greetd + cosmic-greeter, the COSMIC session and applications.
- [ ] `os.hide.Update1` D-Bus API on hideupd; the `hide` CLI moved onto it.
- [ ] COSMIC Settings pages (Updates, Recovery, Extensions) and the update
      applet, in libcosmic.
- [ ] `hidesetup`: first-boot setup.
- [ ] The `workstation` image: `minimal` and the desktop layer.
- [ ] Flatpak with Flathub and `xdg-desktop-portal-cosmic`.
- [ ] `hide shell`: toolbox-style Podman containers.

Done when: the author uses it as the daily workstation for a week without
reaching for another machine. CI boots to the greeter and logs in.

## H6 — NVIDIA and aarch64

- [ ] System extension format, signing and merging; `hide ext`.
- [ ] NVIDIA sysext: open kernel modules per deployment kernel, signed;
      proprietary userspace.
- [ ] aarch64 images published and booting on a physical UEFI ARM machine.

## H7 — Install and recover

- [ ] Installer ISO: partitioning, LUKS enrollment, recovery key, first user.
- [ ] Secure Boot key enrollment, or shim — whichever H0 decided.
- [ ] Recovery UKI: rollback, reinstall keeping `/home`, shell.
- [ ] Release channels `edge`, `beta`, `stable` on a public registry.
- [ ] `hideboot` (in youhide/hideBoot) replaces `systemd-boot`; nothing on the
      ESP changes.

## H8 — Replacing the bridges

Each of these starts only with a written reason why the replacement is better
than what ships, not only that it is Rust.

- [ ] `hidedev`: device manager with a libudev-compatible library.
- [ ] `hidelogin`: the `org.freedesktop.login1` subset COSMIC uses.
- [ ] `busd` instead of dbus-daemon, once it is ready for a desktop.
