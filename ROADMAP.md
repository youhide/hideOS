# Roadmap

Milestones are sequential. Each one ends with something that boots and does
something observable in QEMU — and from H4 on, on real hardware — before the
next one starts. There are no dates.

The design these build toward is in [ARCHITECTURE.md](ARCHITECTURE.md).

## H0 — Foundations

**In progress.**

- [x] Repository, architecture, roadmap.
- [x] Settle what H1–H3 build on: OCI as the update transport, our own boot
      manager in H7 with `systemd-boot` until then. Secure Boot keys vs. shim
      stays open until the installer, which is the first thing it affects.
- [ ] Cargo workspace and `xtask`, with oxinit's lint policy and CI (fmt,
      clippy, tests on Linux and macOS hosts).
- [ ] A Linux build environment reachable from macOS (Lima or a container),
      with QEMU and OVMF for both architectures.

## H1 — hideforge builds a system

The smallest image this project built entirely itself.

- [ ] Recipe format specified in `docs/RECIPE_FORMAT.md`.
- [ ] Hermetic build sandbox: user namespaces, no network, declared inputs
      only, fixed `SOURCE_DATE_EPOCH`.
- [ ] Bootstrap stages 0 → 1 → 2: cross toolchain, native toolchain, final
      system. Nothing from the host in the output.
- [ ] Recipes: Linux, glibc, GCC/LLVM runtime, Rust, uutils, bash, zsh,
      util-linux, kmod, oxinit.
- [ ] Output: a root tree, packed as an initramfs.

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
