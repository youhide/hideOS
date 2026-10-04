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
2. whether to encrypt it.

That is all for the Workstation, as for a Mac: the rest is the first
start's. The installer writes the disk, and an encrypted one gets a random
setup key, kept on the ESP until setup replaces it — hidestage opens the
disk with it, asking nothing. Minimal, which has no screen to set itself up
on, asks for a passphrase, twice, and the first account's login name and
password, and shows the recovery key once.

It installs with the same code as `hide install` (the code `cargo xtask
install` runs to make development disks). See [[Disk and encryption]].

## First-boot setup

On a Workstation no one has set up, greetd shows `hidesetup` instead of the
login screen: full screen, in the greeter's compositor, as the greeter's
user. A page at a time, as a Mac's Setup Assistant:

1. the language — the one the session speaks, and the clock it reads: 24
   hours but where the region uses AM and PM;
2. the keyboard layout, from xkeyboard-config's list;
3. a Wi-Fi network, skipped when a cable is connected;
4. the time zone, from tzdata's `zone1970.tab`;
5. the account: the full name, a login suggested from it, the password —
   an administrator;
6. on an encrypted disk: the password becomes the disk's passphrase, a
   recovery key is made and shown once, and the setup key is taken out of
   the disk and off the ESP.

hidesetup changes nothing itself. Each page is a call to hideupd's
`os.hide.Setup1`, which answers only the greeter's user and root, and only
until `/var/lib/hide/setup-done` exists; the last page writes it, and the
login screen comes up. A machine restarted halfway through starts setup
again, and an account the unfinished setup made is taken back so it can be
made again. Development disks, made with an account, have the mark.

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
