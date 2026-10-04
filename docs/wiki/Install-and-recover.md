# Install and recover

How hideOS gets onto a disk, how it is reinstalled without losing `/home`,
and what is left when nothing on the disk starts. The design is
[ARCHITECTURE.md, "The installer medium"](../../ARCHITECTURE.md#the-installer-medium)
and ["Recovery"](../../ARCHITECTURE.md#recovery). All of it is exercised by
`cargo xtask installer-test`; see [[Testing]]. None of it has run on real
hardware yet.

## The installer medium

```sh
cargo xtask installer                        # Minimal
cargo xtask installer --edition workstation
```

Writes `target/images/installer-EDITION-ARCH.img`, a whole-disk image for a
USB stick rather than an ISO:

| Partition | Contents |
|---|---|
| `hideos-installer` (ESP) | hideBoot as `EFI/BOOT/BOOTX64.EFI`; the installer's UKI in `EFI/Linux`; the recovery system the installer will copy to the disk it installs |
| `hideos-payload` | The edition's `payload.tar`, written raw |

The payload is raw because a tar archive reads the same from a block
device as from a file, and a file on FAT could not be over 4 GiB.

The installer's UKI is Minimal's kernel with Minimal's whole root as a
zstd-compressed initramfs, starting `hide installer` as its init. The
installer is always Minimal's, whichever edition it installs.

## The installer

[crates/hide/src/installer.rs](../../crates/hide/src/installer.rs). On the
console it asks:

1. which disk, from a numbered list, and `erase` typed out to confirm;
2. whether to encrypt it, and a passphrase, twice;
3. the first account's login name and password: an administrator.

It installs with the same code as `hide install` (the code `cargo xtask
install` runs to make development disks), and when the disk is encrypted
shows a recovery key once, to be written down. See
[[Disk and encryption]].

## Reinstalling, keeping `/home`

Pointed at a disk that already holds hideOS, the installer offers to
reinstall rather than erase. It opens the disk with its passphrase or
recovery key and keeps the partitions, the LUKS2 keyslots and `@home`;
everything else is made anew — the ESP, `@store`, `@etc`, `@var`, and
`@swap`, since a kept swap file could hold a hibernated system that is no
longer there to resume. What is replaced is the whole system and its
configuration. The account is asked for again, and a home with its name is
that account's again. It ends with `hideOS is installed. /home is as it
was.`

## The recovery system

`\EFI\Recovery\hideos-recovery.efi` on the ESP: Minimal's kernel and its
whole root as an initramfs, starting `hide recovery`
([crates/hide/src/recovery_system.rs](../../crates/hide/src/recovery_system.rs)).
It runs from memory, so nothing on the disk has to work for it to.

```text
hideOS recovery

  1) Choose the system that starts next
  2) Open a shell, with the disk at /mnt
  3) Restart
  4) Turn off
```

- **Choose the system that starts next** lists the deployments on the ESP;
  the one chosen is marked good and every one that would start before it is
  marked out of attempts — the same renames the boot counter makes. It is a
  rollback a person picks. Nothing is deleted: each stays in hideBoot's
  menu.
- **Open a shell** opens the disk (asking for its passphrase when it is
  encrypted), mounts it at `/mnt` with its subvolumes `@home`, `@etc`,
  `@var` and `@store` inside, and starts zsh. Leaving the shell comes back
  to the menu.

It does not reinstall: it carries no payload, and the store on the disk is
the thing that might be broken. Reinstalling is the installer's.

## hideBoot's menu

Nothing is shown on a normal boot. Holding a key as hideBoot starts opens
the menu: the entries in `\EFI\Linux` newest first, each with what its boot
counter says — being tried, failed to boot, or nothing for a good one —
then the recovery system. Up and Down or the entry's number, then Enter.

The recovery system boots by itself only when no entry in `\EFI\Linux`
will start. It never counts its attempts and is never the default. See
[[Boot chain]] and [youhide/hideBoot](https://github.com/youhide/hideBoot).
