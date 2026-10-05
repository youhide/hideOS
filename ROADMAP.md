# Roadmap

Milestones are sequential. Each one ends with something that boots and does
something observable in QEMU before the next one starts. There are no
dates.

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

**Done on x86_64**, 2026-10-02: `cargo xtask boot --test` boots the kernel and
root hideforge built, with oxinit as PID 1, `hide setup` and a zsh login shell
on glibc, in 4.7 seconds. aarch64 remains: the recipes take `$ARCH`, nothing
has been built for it yet.

The smallest image this project built entirely itself.

- [x] Recipe format specified in `docs/RECIPE_FORMAT.md`, the build model in
      `docs/HIDEFORGE.md`; parsing, validation, dependency graph and input
      hashes in `hideforge-recipe`, tested on any host.
- [x] Hermetic build sandbox: mount, PID, network, UTS and IPC namespaces;
      an overlay whose upper layer is the output; no network; declared
      inputs only; `SOURCE_DATE_EPOCH` from the sources. Checked by
      `cargo xtask forge-selftest`.
- [x] Bootstrap stages 0 → 1 → 2: cross toolchain, native toolchain, final
      system. Nothing from the host in the output: every stage-2 file's
      libraries are checked to come from stage 2, at build and at image time.
- [x] Recipes: Linux, glibc, GCC runtime, Rust, uutils, bash, zsh,
      util-linux, oxinit. (kmod waits for modules; the kernel has what it
      needs built in.)
- [x] Output: hideOS Minimal, a root tree packed as an initramfs, with the
      kernel next to it.
- [ ] The same on aarch64.

Done when: QEMU boots the kernel this project compiled, oxinit is PID 1, and a
console shell runs on a glibc userspace — x86_64 and aarch64.

## H2 — The seal

**Done on x86_64**, 2026-10-03: `cargo xtask seal-test` passes its ten checks
under Secure Boot. `/usr` is read-only; an object replaced by a modified copy
makes reading its file fail with `EIO` ("has no fs-verity digest"); an image
pointed at another sealed file stops the boot in hidestage ("expected
sha256:a1769d…, found sha256:015c11…"); and a UKI changed by one byte — the
only way to change the digest it carries — is refused by the firmware before
anything in it runs. The key is a development key: see ARCHITECTURE.md,
"Security".

- [x] composefs image generation in hideforge, deterministic, digest reported.
- [x] UKI assembly: kernel + initrd + command line with the digest.
- [x] Signing the UKI with a development key, and Secure Boot enforcing it in
      QEMU: `cargo xtask boot --disk --secure-boot`.
- [x] `hidestage`: mounts, GPT discovery, btrfs, composefs with digest
      verification, bind mounts, `switch_root` into oxinit.
- [x] `hide install`: partition, btrfs with its subvolumes, objects with
      fs-verity enabled, ESP with the UKI and `systemd-boot` as the stopgap
      boot manager, the first account. No LUKS yet.
- [x] A disk image, made by booting Minimal in QEMU with an empty disk and
      the payload attached and running `hide install` — the builder's kernel
      cannot enable fs-verity. See ARCHITECTURE, "Disk images are installed,
      not assembled". `cargo xtask install`, then `boot --disk`.
- [x] `cargo xtask seal-test`: the attacks above, scripted, on a disk of
      their own, under Secure Boot — ten checks.

Done when: QEMU boots from a disk into the sealed root; writing to `/usr` fails;
flipping one byte of one object makes reading that file fail with `EIO`; a UKI
with a wrong digest refuses to boot.

## H3 — Updates and rollback

**Done**, 2026-10-05. Updates are OCI images, applied and rolled back:
`cargo xtask update-test` takes Minimal N to N+1 on scratch disks, from
N+1's image as an oci-archive — a good update that boots and is marked
good, and a rollback; an image changed after it was built, refused before
anything is staged; an N+1 that cannot mount, one whose kernel panics and
one that hangs, each tried three times before the machine goes back to N
by itself; and a power cut after each of the update's five steps, which
leaves N booting before the commit and N+1 after. `cargo xtask
registry-test` takes plain `hide update` to the channel's image in a
registry.

- [x] OCI image output from hideforge: the root and the UKI as two layers,
      with the boot digest computed by pulling the image as a client does.
- [x] Pulling and verifying an OCI image in `hide update`, from
      `oci-archive:` or `oci:`: composefs-oci writes the objects the store
      lacks, sealed, and regenerates the boot image, which must have the
      digest the image's own UKI boots.
- [x] Pulling from a registry — `hide`'s own OCI client, checked by
      `cargo xtask registry-test` — and pushing to one from the build:
      `cargo xtask publish` put `minimal-edge` and `workstation-edge` on
      ghcr.io/youhide/hideos.
- [x] Applying an update: the UKI renamed into place with `+3` tries — the
      commit point — and a durable rename, which FAT needs spelled out.
- [x] `hide boot-ok`: mark the booted deployment good once the edition's
      target is reached.
- [x] `hide update`, `hide rollback`, `hide status`.
- [x] Garbage collection of unreferenced objects, at the end of every
      update and as `hide gc`: the roots are the images of the UKIs left on
      the ESP — running, the way back, the new one.

Done when: an update from build N to N+1 applies and boots; a power cut
(QEMU killed) at every step of the update leaves N booting; an N+1 that panics,
one that cannot mount, and one that hangs all end, unattended, back on N.

## H4 — Real hardware

**Done**, 2026-10-05. `cargo xtask net-test`
installs Minimal and finds NetworkManager connected on its own, with DHCP,
DNS, iwd on the bus and every daemon logging to oxlogd; `cargo xtask
power-test` suspends it and lets the clock wake it, then hibernates it and
resumes the same session after QEMU starts again. The encrypted root opens
in hidestage's own Rust: `installer-test` installs it encrypted and boots it
with the recovery key, and `crypt-test` opens it with the TPM.

- [x] eudev, linux-firmware, microcode, with kernel modules: Arch's
      configuration for the kernel; `cargo xtask hw-test` finds udev loading
      a NIC's module, modprobe resolving dependencies, the firmware in the
      image and the microcode leading the initrd.
- [x] LUKS2 in hidestage, with a passphrase or a recovery key; dm-crypt
      mapped by its own ioctls (crates/hidecrypt).
- [x] TPM2 unlock, sealed to PCR 7 at the first boot (`hide tpm-enroll`):
      `cargo xtask crypt-test` opens the disk with swtpm, is refused by it
      when the boot chain changes, and falls back to the passphrase.
- [x] NetworkManager + iwd, ntpd-rs; sudo-rs in the Workstation.
- [x] Suspend and resume, hibernation to the swap file.
- [x] Hardware watchdog fed by oxinit (oxinit M17), with a deadline on
      `boot-ok`: `update-test`'s hanging N+1 is reset by it, three times,
      and N boots.

Done when: hideOS boots from its own disk with the drivers, firmware and
microcode a machine needs, unlocks with TPM2, joins a network, suspends and
resumes, and hibernates.

## H5 — Desktop

**Done**, 2026-10-05. On 2026-10-03 the `workstation` image — 118 recipes, LLVM,
Mesa and COSMIC epoch 1.9 built from source — installed with `cargo xtask
install --edition workstation`, booted sealed to cosmic-greeter, and logged
in to the COSMIC desktop, drawn by llvmpipe on QEMU's virtio-gpu. Since then: polkit, PipeWire for the session, the base
icon theme, a locale for every login, and grep, sed, find and less in
Minimal.

- [x] dbus-daemon, elogind, polkit.
- [x] Mesa (llvmpipe, softpipe, virgl, radeonsi, and Intel's iris, with
      the SPIR-V tools and the LLVM translator).
- [x] PipeWire, WirePlumber and pipewire-pulse, as units of the session's
      own `oxinit --user` (oxinit M18), their output in
      `~/.local/state/oxinit/log`; RealtimeKit for their threads.
- [x] Per-user services in oxinit: `start-cosmic` runs the manager beside
      cosmic-session — `cargo xtask desktop-test` finds its sockets and
      PipeWire's in the session's runtime directory.
- [x] greetd + cosmic-greeter, the COSMIC session and applications.
- [x] `os.hide.Update1` D-Bus API on hideupd (`hide daemon`); the `hide` CLI
      moved onto it, root or polkit to change anything. Checked by
      `cargo xtask update-test`.
- [x] Settings → System → Updates, in libcosmic (a cosmic-settings patch):
      the deployments, going back, the disk's protection and the
      extensions, from hideupd, and "Update now" from the channel. The
      update applet in the panel: "Restart to update" once one is staged.
- [x] `hidesetup`: first-boot setup, as a Mac's — language, keyboard,
      Wi-Fi, time zone, the account, the disk's passphrase and a recovery
      key — in libcosmic, on hideupd's `os.hide.Setup1`. The installer only
      writes the disk; `cargo xtask setup-test` sets up an installed
      Workstation as the greeter and logs in to the account it made.
- [x] The menu bar as a Mac's: hideOS's logo in the corner, opening
      Applications; the power menu by the clock; no coloured frame round
      the focused window.
- [x] The `workstation` image: `minimal-base` and the desktop layer.
- [x] Flatpak with Flathub and `xdg-desktop-portal-cosmic`: Flathub's
      remote from /usr, its signed summary verified with GnuPG, sandboxes
      by bubblewrap with user namespaces — `cargo xtask desktop-test`.
- [x] Dracula, hideOS's default COSMIC theme and terminal colours
      (`tools/cosmic-dracula`); a person changes it in Settings.
- [x] `hide shell`: toolbox-style rootless Podman containers sharing the
      home, the display and the session — `cargo xtask desktop-test` enters
      one as the person and finds the host's files there.

Done when: the Workstation installs, sets itself up at the first start,
boots to the greeter and logs in to the COSMIC desktop, with Flatpak, the
update settings and `hide shell` working — `cargo xtask setup-test` and
`desktop-test`.

## H6 — NVIDIA and aarch64

**In progress.** NVIDIA is done: its driver is a signed extension that the
installer adds and each update brings along. aarch64 remains.

- [x] System extension format, signing and merging; `hide ext`:
      composefs images signed over their digest, merged over /usr by
      hidestage only for the image each was built for, and left out when
      the signature does not hold — `cargo xtask sysext-test`.
- [x] NVIDIA sysext: open kernel modules per deployment kernel, signed;
      proprietary userspace.
  - [x] The extension: open modules built against each image's kernel and
        indexed with its modules, NVIDIA's userspace unmodified, EGL
        behind libglvnd beside Mesa's — `cargo xtask nvidia-test` in QEMU.
  - [x] Updates bring each extension's build for the image they bring,
        and wait while it is missing — `cargo xtask registry-test`.
- [ ] aarch64 images published and booting in QEMU's UEFI `virt` machine.

## H7 — Install and recover

**Done**, 2026-10-05: the installer, recovery, hideBoot with hideOS's own
Secure Boot keys, hideOS beside Windows with a Startup Disk, and image 49
on all three channels.

- [x] Installer medium (a disk image for a USB stick rather than an ISO):
      partitioning, LUKS2 with a recovery key, the first user —
      `cargo xtask installer`, tested by `cargo xtask installer-test`.
- [x] Secure Boot with hideOS's own keys, Microsoft's certificates beside
      them: the installer enrolls them on a firmware in setup mode, and
      `cargo xtask secureboot-test` boots under them and is refused a
      changed UKI.
- [x] Recovery system on the ESP, from hideBoot's menu or when nothing else
      starts: choose what boots next, or a shell. Reinstall keeping `/home`
      is the installer's, which carries a payload — see ARCHITECTURE,
      "Recovery". Tested by `cargo xtask installer-test`.
- [x] Release channels `edge`, `beta`, `stable` on a public registry.
  - [x] ghcr.io/youhide/hideos, public; `cargo xtask publish` to edge, the
        image's extensions first.
  - [x] `cargo xtask promote --to beta|stable`: a channel takes the image
        the one before it has, never a rebuild, once its extensions are
        published.
  - [x] A build on beta: image 49, Minimal and Workstation, 2026-10-05.
  - [x] A build on stable: image 49, 2026-10-05.
- [x] Beside Windows, as Boot Camp (ARCHITECTURE, "Beside Windows"): the
      installer says what each disk holds and installs on a disk of its
      own or in the free space beside Windows, with its own ESP and its own
      entry first in the firmware's boot order; hideBoot offers Windows;
      BitLocker found and warned about before Secure Boot keys change; the
      hardware clock kept in local time beside Windows — `cargo xtask
      beside-test`, on a disk laid out as Windows lays out its own, with
      hideBoot's stand-in for Windows Boot Manager.
- [x] "Startup Disk": the default system, and "Restart in Windows", from
      Settings and `hide startup-disk` — `beside-test` restarts in Windows
      once through hideupd, as Settings does, and the next start is
      hideOS's.
- [x] The installer adds the nvidia extension on a machine with an NVIDIA
      GPU, from the medium: `hide install --extensions` adds it with the
      system — `cargo xtask nvidia-test`; the GPU is found by its PCI ID.
- [x] `hideboot` (in youhide/hideBoot) replaces `systemd-boot`; nothing on the
      ESP changes. `update-test` and `seal-test` pass with it, Secure Boot
      included.

## H8 — Replacing the bridges

Each of these starts only with a written reason why the replacement is better
than what ships, not only that it is Rust.

- [ ] `hidedev`: device manager with a libudev-compatible library.
- [x] `hidelogin`: the `org.freedesktop.login1` subset COSMIC uses, in place
      of elogind (2026-10-05). The reason is in ARCHITECTURE, "hidelogin":
      sessions are cgroups delegated to their user, shutdown is oxinit's,
      the power key is COSMIC's. One daemon serves login1 and seatd's
      protocol; a PAM module registers sessions; `libhidelogin-sd` answers
      sd-login for polkit, NetworkManager and WirePlumber; `loginctl`
      locks, suspends and powers off; the lid suspends. Tested by
      `cargo xtask desktop-test` — the session on seat0 in its own cgroup,
      active for polkit; the power button's countdown powering off; suspend
      — and by every test that logs in at the greeter.
- [ ] `busd` instead of dbus-daemon, once it is ready for a desktop.
