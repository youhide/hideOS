# Updates and rollback

An update is a new image next to the old one, never a change to the
running one. It becomes bootable at one instant, the rename of its UKI on
the ESP, and the boot manager gives it three attempts. The design is
[ARCHITECTURE.md, "Updates"](../../ARCHITECTURE.md#updates); the code is
[crates/hide/src/deploy.rs](../../crates/hide/src/deploy.rs). It is checked
end to end by `cargo xtask update-test`; see [[Testing]].

## The image

hideforge publishes the system as an OCI image with two layers: the root,
then `/boot/EFI/Linux/<uki>`. The digest the UKI carries is that of the
*boot* composefs image — the root with `/boot` emptied — so the UKI can
travel inside the image it seals. hideforge computes that digest by pulling
its own image with composefs-oci, the code `hide update` runs, and fails the
build if adding the UKI layer changed it. `cargo xtask image` writes it as
`image.oci.tar`.

It is published to <https://ghcr.io/youhide/hideos>, a public registry,
one tag per edition and channel: `minimal-edge`, `workstation-beta`, …
Plain `hide update` pulls this machine's edition on the channel
`/usr/lib/hide/update.conf` names — `edge` while hideOS is pre-alpha —
which `/etc/hide/update.conf` overrides. `hide update --image` also takes
`oci-archive:PATH` and `oci:DIR[:TAG]`, which is how the tests feed it.

## Channels

`edge` is every build that passes the round of tests; `beta` and
`stable` take an image the channel before them already has, never a
rebuild, so what a beta machine runs is what edge machines ran:

```sh
cargo xtask publish --edition workstation --channel edge
cargo xtask promote --edition workstation --to beta     # edge's image
cargo xtask promote --edition workstation --to stable   # beta's image
```

An image's [[System extensions]] are published under its digest before its
channel tag moves, and `promote` refuses an image whose extensions are not
there: an update brings the extensions with it, and would wait for one
that is missing. ARCHITECTURE has stable wait for beta's boot-success
telemetry; until that exists, promoting is the maintainer's call.

## `hide update`

```sh
hide update --image oci-archive:/path/to/image.oci.tar
```

Five steps, which are also the names `--crash-after` takes in tests:

| Step | What it does |
|---|---|
| `pull` | composefs-oci writes the objects the store lacks, sealed with fs-verity, and regenerates the boot image. The UKI's `hideos.image=` must name that image, and the kernel must measure it the same; otherwise nothing is staged. Files the new `/etc` has and the machine's lacks are added; nothing in `/etc` is replaced. |
| `stage` | The UKI is copied to `EFI/Linux/<name>.tmp` on the ESP and synced. |
| `commit` | The rename to `hideos-EDITION-VERSION-DIGEST12+3.efi`. From here the next boot tries the new system. |
| `prune` | UKIs no longer kept are removed: kept are the running one, the newest good one other than it (the way back), and the new one. |
| `collect` | Garbage collection, below. |

It refuses an image that is the running system, and an image of another
edition: switching edition is meant to be `hide rebase`, which is not
written yet. The client never trusts an image it did not compute itself:
the build signed the UKI, the firmware checks the UKI, and the UKI names
the digest.

**A power cut at any instant** leaves a machine that boots: the old system
before the commit, the new one with its attempts after it. Renames on the
ESP sync the file under its new name, its directory and the filesystem,
because a plain rename on FAT can leave a zero-length file after a cut.

## Boot counting and automatic rollback

The new UKI starts at `+3`. hideBoot renames it before each attempt
(`+2-1`, `+1-2`, `+0-3`); at `+0` it sorts after every good entry and the
previous deployment boots. An attempt fails when:

- hidestage refuses or cannot mount the image, says why, and reboots;
- the kernel panics: `panic=10` on the command line reboots it;
- the boot hangs anywhere, kernel or userspace: hidestage armed the
  hardware watchdog (180 s, `hideos.watchdog=`), and only `hide boot-ok`
  stops it.

`hide boot-ok` runs at the end of the edition's target and renames the UKI
without its counter: the deployment is good, and the update permanent. See
[[Boot chain]] for the file names.

## The other commands

```sh
hide status              # the deployments, in the order they boot
hide status --porcelain  # the same, tab-separated, for hideupd
hide rollback            # boot the previous deployment next
hide gc                  # garbage collection by hand
```

`hide status` lists version, state (`good`, `trying` or `bad`), the digest,
which one is running and which boots next, then how the disk is protected.
`hide rollback` marks the running deployment out of attempts, so the boot
manager passes over it; it refuses when there is nothing else to go back
to.

## Garbage collection

At the end of every update, and as `hide gc`: every object in the store that
no kept image uses is deleted. The roots are the images of the UKIs on the
ESP — running, the way back, the new one — and the images of the system
extensions. The ESP is the list of what can boot, so it is also the list of
what must stay. A collection cut short leaves unused objects for the next
one.

## hideupd and `os.hide.Update1`

hideupd is `hide daemon`, started on every edition by the `hideupd` unit in
[hideos-units](../../recipes/system/base/hideos-units). It owns
`os.hide.Update1` on the system bus, at `/os/hide/Update1`
([crates/hide/src/daemon.rs](../../crates/hide/src/daemon.rs)).

Every operation is `hide` itself, run as a child process with `HIDE_DIRECT=1`
set so that it does the work rather than ask the daemon: the code a root
shell runs is the code the daemon runs, and an operation that fails takes
nothing of the daemon with it.

| Member | Kind | What it does | Who may |
|---|---|---|---|
| `Update(s image) → u job` | method | `hide update --image IMAGE` | root, or polkit `os.hide.update.update` |
| `Rollback() → u job` | method | `hide rollback` | root, or `os.hide.update.rollback` |
| `AddExtension(s image) → u job` | method | `hide ext add IMAGE` | root, or `os.hide.update.extensions` |
| `RemoveExtension(s name) → u job` | method | `hide ext remove NAME` | root, or `os.hide.update.extensions` |
| `Collect() → u job` | method | `hide gc` | root, or `os.hide.update.update` |
| `Status() → s` | method | what `hide status` prints | anyone |
| `Deployments() → a(stssbb)` | method | edition, version, digest, state, running, boots next | anyone |
| `Disk() → s` | method | how the disk is protected | anyone |
| `Extensions() → s` | method | what `hide ext list` prints | anyone |
| `Busy` | property, `b` | whether a job is running | anyone |
| `Progress(u job, s line)` | signal | a line of the job's output | |
| `Finished(u job, b ok, s error)` | signal | the job's end, and why it failed | |

A method that starts a job returns its number at once; the job is followed
by its signals, and a client listens before it asks, since a job can end
before the call returns. One job runs at a time.

**Who may.** Root may ask for anything. Anyone else is asked about by
polkit, with the actions in
[crates/hide/data/os.hide.update.policy](../../crates/hide/data/os.hide.update.policy):
an administrator's password, for an active session at the machine, kept
for a while. Minimal has no polkit, so there only root may.

**The command line is a client.** `hide update`, `rollback`, `gc` and `ext
add|remove` go through hideupd when it owns its name on the bus, printing
its `Progress` lines as they come
([crates/hide/src/client.rs](../../crates/hide/src/client.rs)). Where there
is no daemon — the installer, the recovery system, a boot that has not
reached it — they do the work themselves. Either way an exclusive lock on
`/run/hide.lock` keeps it to one change at a time.

**Settings.** The Workstation's COSMIC Settings has a System → Updates page,
a hideOS patch to
[cosmic-settings](../../recipes/system/cosmic/cosmic-settings.toml), that
shows what `Deployments` reports, goes back to the previous system, and
shows the disk's protection and the system extensions.
