# hideOS wiki

hideOS is a Linux workstation operating system built from source and
sealed: the system is one read-only image, checked by fs-verity through
composefs, whose digest is carried on the command line of a signed kernel
image. Updates replace the image whole, and an update that does not boot
is rolled back by the boot manager. [oxinit](https://github.com/youhide/oxinit)
is PID 1 and COSMIC is the desktop.

This wiki is the working notes: how to build, test and reason about the
tree as it is today. The design itself is specified in
[ARCHITECTURE.md](../../ARCHITECTURE.md), and what is done in
[ROADMAP.md](../../ROADMAP.md); where the wiki and those disagree, they
decide, and the wiki is wrong.

## Status

Pre-alpha, and only in QEMU, on x86_64:

- hideOS **Minimal** (a shell) and **Workstation** (the COSMIC desktop)
  build from source with hideforge, and boot sealed from a disk that
  hideOS's own installer wrote.
- The boot chain is UEFI, [hideBoot](https://github.com/youhide/hideBoot),
  a signed UKI, hidestage, oxinit. Secure Boot is enforced with a
  development key.
- Updates are OCI images. A good update boots and is marked good; one that
  cannot mount, panics or hangs is tried three times and then the previous
  system boots. A power cut after any of an update's five steps leaves a
  system that boots.
- The root can be LUKS2-encrypted, opened in hidestage by passphrase,
  recovery key, or a key sealed to the TPM.
- Signed system extensions merge over `/usr` at boot.
- A recovery system on the ESP chooses what boots next or opens a shell;
  the installer can reinstall keeping `/home`.
- The Workstation has Flatpak with Flathub (its signed summary verified),
  the desktop portals, hideupd on the system bus, and a Settings → Updates
  page; its default theme is Dracula.

Not yet: real hardware, updates from a registry, aarch64 images, Secure
Boot with hideOS's own keys. The milestones are H0 to H8 in
[ROADMAP.md](../../ROADMAP.md).

## Pages

- [[Building]]: the builder container, `cargo xtask image`, editions, where
  the outputs go.
- [[Testing]]: every `cargo xtask *-test`, what it proves and what it needs.
- [[Boot chain]]: from the firmware to oxinit, and what each step checks.
- [[Updates and rollback]]: OCI images, boot counting, garbage collection,
  hideupd and `os.hide.Update1`.
- [[Disk and encryption]]: partitions, subvolumes, LUKS2, the TPM, swap and
  hibernation.
- [[Install and recover]]: the installer medium, reinstalling, the recovery
  system.
- [[System extensions]]: what they are, how they are signed, `hide ext`.
- [[Applications]]: Flatpak, Flathub, the portals.
- [[hideforge]]: recipes, the sandbox, the bootstrap stages.
- [[Development notes]]: lessons that were not obvious, one entry each.

## Editing the wiki

The pages are Markdown in [docs/wiki](../wiki), one file per page, and
change in the same commit as the work they describe. `cargo xtask wiki`
renders them into `site/wiki/`, and `cargo xtask publish-site` does the
same before it publishes. See [docs/wiki/README.md](README.md) for the
conventions.
