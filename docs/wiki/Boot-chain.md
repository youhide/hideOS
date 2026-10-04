# Boot chain

From power-on to the login prompt, each step checks the next before it
runs it. The design, with its reasons, is
[ARCHITECTURE.md, "Boot chain"](../../ARCHITECTURE.md#boot-chain); this
page is what the tree does today.

```text
UEFI firmware (Secure Boot)
  └─ hideBoot                 \EFI\BOOT\BOOTX64.EFI, signed
      └─ the UKI              \EFI\Linux\hideos-…efi, signed: kernel + initrd + command line
          └─ hidestage        the initrd's /init: unlock, resume, check the seal, assemble
              └─ oxinit       PID 1, from the sealed /usr
```

## Firmware and Secure Boot

The firmware loads the boot manager from the ESP and, with Secure Boot on,
refuses it unless it is signed by a key in its database. hideBoot then
loads the UKI through the firmware's own `LoadImage`, so the UKI's
signature is checked the same way.

**The key is a development key.** `cargo xtask image` signs hideBoot and
the UKI with Debian's "snakeoil" key, which Debian's OVMF ships already
enrolled, with the private half published so that anyone can sign for it.
That proves the mechanism — the firmware refuses a changed or unsigned
kernel image — not who made the image; no machine outside QEMU trusts it.
Whether hideOS enrolls its own keys or uses shim is still **Proposed**; see
[ARCHITECTURE.md, "Security"](../../ARCHITECTURE.md#security).

`cargo xtask boot --disk --secure-boot` boots with firmware that enforces
it, copied out of the builder into `target/firmware/`. x86_64 only for now.
On a Mac those boots run on QEMU's emulated CPUs: x86 Secure Boot needs
System Management Mode, which macOS's hypervisor lacks. hidestage prints
`hidestage: secure boot on|off` so a test can tell.

## hideBoot

[youhide/hideBoot](https://github.com/youhide/hideBoot), a UEFI
application in Rust, built from source by the
[hideboot recipe](../../recipes/system/boot/hideboot.toml) (0.2.0). It:

1. lists the UKIs in `\EFI\Linux\`, newest version first;
2. skips those whose attempts are used up;
3. renames the one it picks to count the attempt, and boots it;
4. sets systemd-boot's `LoaderBootCountPath` and `LoaderEntrySelected`, so
   the system knows which file to rename when it marks the boot good;
5. shows a menu only while a key is held as it starts, drawn at the
   display's native resolution as its EDID reports it;
6. lists the recovery system from `\EFI\Recovery\` last in the menu, and
   boots it by itself only when nothing in `\EFI\Linux\` will start.

It knows nothing of hideOS beyond that file-name convention, which is also
systemd-boot's: `hideforge image` still puts systemd-boot on the ESP when
not given `--boot-manager hideboot`, which `cargo xtask` always passes.

## Boot counting, in file names

Each deployment is one UKI on the ESP, named

```text
hideos-EDITION-VERSION-DIGEST12[+LEFT[-DONE]].efi
```

`DIGEST12` is the first twelve hex digits of the image digest the UKI
boots; `VERSION` is the image version, the commit count of the tree it was
built from. The suffix is the Boot Loader Specification's boot counting:

| Name | Meaning |
|---|---|
| `…+3.efi` | Just staged by an update: three attempts left. |
| `…+2-1.efi` | Renamed by the boot manager before its first attempt. |
| `…+0-3.efi` | Out of attempts: sorts after every good entry, so the previous deployment boots. |
| `…efi` with no suffix | Booted to completion and marked good by `hide boot-ok`. |

Code: [crates/hide/src/deployment.rs](../../crates/hide/src/deployment.rs).
What makes an attempt fail is on [[Updates and rollback]].

## The UKI

One PE file per deployment: the systemd EFI stub with the kernel, the
initrd, the command line and `os-release` added as sections, assembled with
objcopy ([crates/hideforge/src/uki.rs](../../crates/hideforge/src/uki.rs)).
Because the file is signed, its command line is too, and the command line
carries the image digest:

```text
console=ttyS0 console=tty0 hideos.image=sha256:<64 hex digits> panic=10
```

- `hideos.image=` is the fs-verity digest of the system's EROFS image: the
  seal.
- `panic=10` turns a kernel panic into a reboot, which the boot manager
  counts as a failed attempt.
- `console=tty0` last makes the screen `/dev/console`. Images built by
  `cargo xtask` append `console=ttyS0` for the tests; see
  [[Development notes#the-screen-is-the-console-tests-append-the-serial-port]].
- hidestage also reads `hideos.root=` (the root partition's name, default
  `hideos-root`), `hideos.init=` and `hideos.watchdog=` (seconds, default
  180, `0` for none).

## hidestage

The initrd's `/init`, in Rust, under the boot-path rules in
[CLAUDE.md](../../CLAUDE.md): no panics, errors as values, `unsafe` only in
its `sys` module. In order
([crates/hidestage/src/boot.rs](../../crates/hidestage/src/boot.rs)):

1. Mount `/proc`, `/sys`, `/dev`, `/run`; report Secure Boot's state.
2. Read the command line.
3. Wait for the partition named `hideos-root`.
4. If it is LUKS2, open it — the TPM's key, else a passphrase on the
   console — and map it with dm-crypt. See [[Disk and encryption]].
5. Resume a hibernated system if there is one, before anything mounts the
   disk.
6. Arm the hardware watchdog, and let go of it running.
7. Mount btrfs, open the image named by the digest, and check its
   fs-verity digest against the command line: a mismatch is a refusal,
   `seal ok` is printed otherwise.
8. Mount the image as composefs with `verity=require`, so every file's
   content is checked against the digest the image records for it.
9. Merge the [[System extensions]] signed for this image.
10. Bind `@etc`, `@var`, `@home` (and `@swap` where it exists); mount a
    tmpfs on `/tmp`.
11. `switch_root` into the system and exec oxinit.

When something cannot be done, hidestage says what and why on the console,
waits 30 seconds and reboots, which the boot manager counts as a failed
attempt of this deployment.

## oxinit, and the end of a boot

[oxinit](https://github.com/youhide/oxinit) is PID 1. Before the services
that need them, `hide setup` makes `/etc/machine-id`, the system users from
`sysusers.d` and the paths from `tmpfiles.d`. Each edition's `default`
target pulls in `boot-ok`, which runs `hide boot-ok` once the edition is up
(the `multi-user` target for Minimal, `graphical` for Workstation): it
renames the UKI without its counter and stops the watchdog. Until
then, a boot that hangs anywhere is reset by the watchdog and counted as a
failed attempt.
