# Disk and encryption

How `hide install` lays out a disk, how hidestage opens it, and where
hibernation goes. The design is in
[ARCHITECTURE.md, "Disk layout"](../../ARCHITECTURE.md#disk-layout) and
["Security"](../../ARCHITECTURE.md#security); the code is
[crates/hide/src/install.rs](../../crates/hide/src/install.rs),
[crates/hidestage/src/unlock.rs](../../crates/hidestage/src/unlock.rs) and
[crates/hidecrypt](../../crates/hidecrypt).

## Partitions

GPT, with partition types from the Discoverable Partitions Specification:

| # | GPT name | Size | Contents |
|---|---|---|---|
| 1 | `hideos-esp` | 512 MiB | FAT32: hideBoot, one UKI per deployment, the recovery system, the TPM-sealed key |
| 2 | `hideos-root` | the rest | btrfs, or LUKS2 with btrfs inside |

hidestage finds the root by its GPT name. ARCHITECTURE's table plans a
2 GiB ESP; `hide install` makes 512 MiB today.

## Subvolumes

| Subvolume | Mounted at | Contents |
|---|---|---|
| `@store` | `/hideos` | The composefs store: objects by fs-verity digest, the EROFS images, extension records |
| `@etc` | `/etc` | What the machine changed; the factory `/etc` from the payload at install |
| `@var` | `/var` | State, logs (`/var/log/oxinit`), Flatpak installations |
| `@home` | `/home` | People's files |
| `@swap` | `/swap` | The swap file, when there is one |

Everything else is the sealed image, read-only. So the few places that
must be writable are made so on purpose: `/tmp` is a tmpfs mounted by
hidestage; root's home is `/var/root`; `/var/run` and `/var/lock` are links
into `/run`, made by `hide setup` from `tmpfiles.d`.

## LUKS2

`hide install --encrypt PASSPHRASE` (and the installer, which asks) formats
the root as LUKS2 with cryptsetup, labelled `hideos`. Writing a header is
cryptsetup's, at install time.

Opening it is hideOS's own: hidecrypt reads the LUKS2 header and its JSON
metadata, derives the keyslot's key, merges the anti-forensic split and
checks the digest — the steps `cryptsetup open` takes — and hidestage maps
the root with device-mapper's ioctls. Nothing written in C runs in the
initrd. hidecrypt is a library tested on any host, against a header
cryptsetup wrote (`crates/hidecrypt/tests`).

hidestage tries, in order:

1. the key sealed to the TPM, if there is a TPM and a sealed key;
2. a passphrase typed on the console, at `Passphrase for the hideOS disk:`.
   The recovery key is a passphrase too, in another keyslot.

## The recovery key

The installer adds a second keyslot opened by a recovery key: 200 random
bits in Crockford's base32 — no I, L, O or U — as eight groups of five,
shown once, to be copied by hand
([crates/hide/src/recovery.rs](../../crates/hide/src/recovery.rs)). It
opens the disk when the passphrase is forgotten and the TPM cannot.

## TPM2

`hide tpm-enroll` seals the volume key to the TPM. The key comes from the
running dm-crypt table, so no keyslot is added and the header is not
written. The sealed blob goes on the ESP as `EFI/hideos/root.tpm2`: it is
not secret, since only that TPM, in that boot state, opens it.

- **The policy is PCR 7**: Secure Boot's state and its keys. An update does
  not change it, so updates need no new seal. Turning Secure Boot off or
  enrolling other keys does, and then the disk asks for its passphrase.
  The signed PCR 11 policy in ARCHITECTURE comes with hideOS's own keys.
- **At every boot** the `tpm-enroll` unit runs `hide tpm-enroll
  --if-missing`: it seals only when the root was opened by passphrase
  because nothing was sealed. When the TPM *refused* a sealed key — the boot
  chain changed — it says so and does nothing; a new seal is for the person
  who knows why it changed.
- **Not yet**: the session is not salted, so the key crosses the bus to a
  discrete TPM in the clear. Firmware TPMs have no bus; parameter encryption
  is the next step.

The TPM commands are marshalled by hidecrypt itself
([crates/hidecrypt/src/tpm2.rs](../../crates/hidecrypt/src/tpm2.rs)).
`hide status` says how the disk was opened and whether a key is sealed.
`cargo xtask crypt-test` drives all of this with swtpm; see [[Testing]].

## Swap and hibernation

`hide install` makes `/swap/swapfile` with `btrfs filesystem mkswapfile`, as
large as the machine's memory unless `--swap MIB` says otherwise (`0` for
none), because hibernation writes memory there.

The kernel needs to know where the file is twice: when it hibernates, and,
before anything is mounted, when the next boot resumes. The kernel command
line cannot say it — it is signed and the same for every machine — so:

1. at every boot the `swap` unit runs `hide swap`: swap on (which also
   rewrites the signature of an image never resumed, so a stale one is
   never found later), then `/sys/power/resume` and `resume_offset`, then
   the same offset in an EFI variable of hideOS's own, `HideosResume`;
2. hidestage reads that variable and asks the kernel to resume before it
   mounts the disk, since writing to a hibernated system's filesystems
   first would lose its writes.

Suspend to RAM is elogind's, or `/sys/power/state`. `cargo xtask
power-test` suspends and wakes, hibernates and resumes; see [[Testing]].
