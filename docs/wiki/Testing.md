# Testing

A milestone is done when it boots and is checked, not when it compiles.
Most of the checks are `cargo xtask` commands that build an image, install
it on a scratch disk in QEMU, boot it, and type at its serial console. The
command list is `cargo xtask help`; the code is in
[crates/xtask/src/main.rs](../../crates/xtask/src/main.rs).

## Before pushing

What CI runs on a release, and nothing slow:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask firmware-smoke --arch x86_64
cargo xtask firmware-smoke --arch aarch64
```

CI runs only for `v*` tags or by hand, never on push; see
[CONTRIBUTING.md](../../CONTRIBUTING.md#ci).

## What every boot test shares

- **It builds what it tests.** Each test runs `cargo xtask image` first, in
  the builder, so it tests this tree. With nothing changed that is a few
  seconds; after a change to a recipe or to a crate that ships, it is that
  rebuild. hideforge stamps the image directory with what the image was
  assembled from (`.hideforge-image`), and leaves an image assembled from
  the same inputs as it is. Two tests in one checkout never run at once:
  see [[Development notes#one-image-build-at-a-time-per-checkout]].
- **It installs a fresh disk** the way `cargo xtask install` does, by
  booting Minimal from RAM with `hide install` as PID 1. An install may take
  up to 15 minutes before the test gives up.
- **A failed boot ends QEMU.** QEMU runs with `-no-reboot`, so a guest that
  reboots — after a panic, a watchdog reset, hidestage giving up — exits,
  and the test starts it again when it wants the next attempt.
- **It reads and types on the serial console**, which is `/dev/console` in
  test images; see [[Development notes#the-screen-is-the-console-tests-append-the-serial-port]].
- **Each check prints `ok` or `FAIL`** with the console output that made it
  fail. The test fails if any check did.
- **It needs** QEMU with UEFI firmware on the host (`cargo xtask doctor`)
  and the builder. On a Mac, two kinds of boot run on QEMU's emulated CPUs
  rather than macOS's hypervisor, and take noticeably longer: Secure Boot,
  because x86 firmware keeps its variables in System Management Mode, which
  the hypervisor does not offer; and a machine with an emulated TPM, whose
  firmware stalls under the hypervisor before it prints anything.

Durations below are given as what a test does — builds, installs, boots —
because few runs have been timed. Recorded numbers are marked.

## A round of tests

```sh
cargo xtask round                 # every test, in lanes side by side
cargo xtask round --lanes 2 beside-test installer-test --edition workstation
```

Each lane is a copy of the checkout in `target/lanes/N`, with its own
`target/`, and runs its tests one after another; the lanes run at once.
The images are built once, before the lanes, and each lane gets them as
hard links, so that four lanes do not assemble four Workstations at once
on one disk. Logs go to `target/logs/round-<test>.log`, one line per test
to `target/logs/round.log`, and every guest's serial console as it comes
to the lane's `target/logs/console-<pid>.log` — where to look while a test
waits for a line. The default is one lane per two CPUs, at most three.

On an eight-CPU Linux PC with KVM, a guest boots in seconds where the
Mac's emulated Secure Boot takes minutes; `hw-test` took 2 minutes there
and 15 on the Mac.

## The tests

### `firmware-smoke [--arch ARCH]`

Boots the UEFI firmware with no disk and checks it reaches boot device
selection. Every later boot stands on this. Recorded: 1.6 s on x86_64
(HVF) and 6.8 s on aarch64 (TCG) on an Intel Mac. Needs only QEMU and its
firmware.

### `forge-selftest`

Builds the fixtures in `crates/hideforge/tests/recipes` in a scratch work
directory and checks each guarantee the build sandbox makes: no network
and a read-only builder for host builds; only declared inputs for target
builds; an output that is exactly what the build created; a later stage
replacing an earlier one's files, and the same stage refused; unchanged
inputs not rebuilt; a failed build leaving no store path and no build
directory. Needs the builder. See [[hideforge]].

### `boot --test`

Boots Minimal from RAM and waits for the banner unit's line, `hideOS:
booted on`: the kernel booted, oxinit is PID 1 and ran a program on the
glibc userspace. Recorded: 4.7 s (H1). `--disk` boots `disk.raw` through
UEFI instead.

### `seal-test`

H2's checks, on a disk of their own, under Secure Boot: it boots from the
sealed disk; the firmware enforced Secure Boot; writing to `/usr` fails and
`/etc` stays writable; an object in the store replaced by a modified copy
makes reading its file fail with `EIO`; the image swapped for another
sealed file is refused by hidestage; a UKI changed by one byte — changed
from Minimal booted beside the disk — is refused by the firmware before
anything in it runs. One install and four boots: three of the disk under
Secure Boot, one of Minimal from RAM beside it.
x86_64 only: there is no Secure Boot firmware for aarch64 here yet.

### `update-test`

H3's "done when". It builds N from this tree and N+1, the same system one
version later, with a 45-second watchdog so that the hang costs minutes.
N+1 is handed to the guest as an oci-archive on a second disk. Then, each
on a freshly installed disk:

- a good update: staged through hideupd (which refuses anyone but root),
  collecting an orphaned object, N+1 boots and is marked good, `hide
  rollback` goes back to N;
- an N+1 whose image cannot be mounted, one that hangs, one whose kernel
  panics: each tried three times, then N boots by itself and N+1 is marked
  bad;
- an image changed after it was built: refused, nothing staged;
- a power cut after each of the five steps (`pull`, `stage`, `commit`,
  `prune`, `collect`): N boots before the commit, N+1 after.

Ten installs and dozens of boots: the longest test. To chase one power
cut, `HIDEOS_UPDATE_TEST_STEPS=commit,prune` (or `tamper`) runs only those,
and keeps the disk to look at. See [[Updates and rollback]].

### `net-test`

Installs Minimal and checks that NetworkManager connects the wired port by
itself, writes `/etc/resolv.conf` with the DHCP server's DNS, that names
resolve, that iwd answers on the bus, and that NetworkManager logs to
oxlogd rather than the console. QEMU's user network gives DHCP and DNS;
resolving a name needs the host to be online. One install, one boot.

### `power-test`

Installs Minimal, checks the swap file is on and the kernel knows where to
hibernate, suspends it and lets the clock wake it (`rtcwake`), then
hibernates it, starts QEMU again, and checks the same session is back —
a file in `/tmp`, which is memory only, still there. One install, two boots.

### `crypt-test`

Installs Minimal encrypted and checks: the root is not readable on the
disk; it asks for the passphrase and refuses a wrong one; the first boot
seals the key to the TPM; the second boots without asking; with Secure Boot
turned on, PCR 7 changes, the TPM refuses, it asks, and the passphrase
still opens it; with no TPM, it asks. **Needs `swtpm` on PATH** (`brew
install swtpm`), which runs on the host and gives QEMU its TPM. One
install, four boots; on a Mac the three with a TPM run emulated. ROADMAP H4 records whether the TPM path has been run.

### `sysext-test`

Builds `hello-sysext` (in `recipes/system/test`) as a system extension
three ways and checks, on an installed Minimal: an unsigned one is refused
by `hide ext add`; one built for another image is added but left out at
boot; one signed for this image is merged over `/usr`, read-only; and that
one is left out once its record's signature is damaged. One install, four
boots. See [[System extensions]].

### `registry-test`

Builds Minimal N and N+1 and the test extension for each, and serves N+1
and N+1's extension from a small read-only registry of xtask's own. On an
installed N with the extension for N: plain `hide update` waits while the
registry has no extension build for N+1; once it has, the update fetches
the image and the build and commits; N+1 boots with its build merged; and
after `hide rollback`, N boots with N's. See [[Updates and rollback]] and
[[System extensions]].

### `nvidia-test`

Builds the Workstation and the nvidia extension for it, and installs the
two together, as the installer does from its medium on a machine with an
NVIDIA GPU (`hide install --extensions`; finding the GPU is tested on the
host, QEMU has none to show). Then it checks, in QEMU: its
modules are the running kernel's and are indexed with the image's;
`nvidia.ko` loads as far as finding no GPU; NVIDIA's libraries and
`nvidia-smi` link, its EGL vendor file sits beside Mesa's in libglvnd's,
and modprobe.d keeps nouveau off; and the desktop still comes up, drawn by
Mesa through libglvnd. See [[System extensions]].

### `beside-test`

Lays out a disk as Windows 11 lays out its own — the ESP with a stand-in
for `bootmgfw.efi`, the reserved partition, an NTFS system volume, the
recovery partition — with 40 GiB free after them, made by Minimal from RAM.
Then the installer: it says the disk holds Windows, offers the space beside
it, installs there; the disk boots from the firmware's own entry for
hideOS, first in its order; Windows's partitions, their places and
contents are as they were; and the hardware clock is read as local time.
See [[Install and recover]].

### `installer-test`

Builds an installer medium and drives it as a person would: boots it beside
an empty disk, answers its questions on the console, waits for it to
install and show a recovery key; boots the installed disk, opened with that
key; opens hideBoot's menu with a held key, starts the recovery system and
chooses what starts next; then boots the medium again and reinstalls over
the disk, keeping `/home`, and checks a file left in the home is still
there, the account's. See [[Install and recover]].

### `desktop-test`

Builds and installs the Workstation and checks, at its console, what it
adds to Minimal: hideupd answers `os.hide.Update1.Deployments` on the system
bus with the running deployment; polkit knows hideupd's actions; Flathub is
configured with no file in `/etc` and its signed summary verifies (needs
the host online); bubblewrap makes an unprivileged sandbox for a user; the
portals, Flatpak's system helper and Settings' Updates page are in the
image. Then it logs in at the greeter and checks the desktop: an
application started in the session stays up; the session is hidelogin's,
on seat0, in a cgroup its user owns, and active as polkit sees it; `hide
shell` enters a container; polkit asks an administrator's password and
takes it; the power button asks rather than powering off, and the
machine powers off by itself at the end of COSMIC's countdown, within a
minute, with the session up. A second boot logs in again and checks that
COSMIC's Suspend action suspends and the clock wakes the machine — last,
because after QEMU's S3 resume virtio-gpu's commits stall and a session
can no longer end cleanly. The first Workstation build is most of a day; see
[[Building]].

### `setup-test`

Installs the Workstation as the installer does — encrypted, with a setup
key and no account — and checks first-boot setup: the setup key opens the
disk with nothing asked; hidesetup is on the screen as the greeter's user;
it offers languages, layouts and zones; anyone but the greeter is refused,
and a zone outside `/usr/share/zoneinfo` too. Then, as the greeter, each
page's call: the language, keyboard, time zone and account are taken; the
account is an administrator with its home, keyboard, clock and subordinate
IDs; the disk takes the passphrase and gives a recovery key, both open it
and the setup key no longer does; setup finishes and answers no one after.
The login screen lets the new account in, in its language, and the next
boot asks for the passphrase and opens with it. Saves `setup-test.png`,
`setup-test-greeter.png` and `setup-test-desktop.png`.

## Pictures

Not tests, but real boots, and how the site's and the READMEs' pictures
are made:

- `screenshot [--edition E] [--login]`: boots the edition with a display,
  types `cat /etc/os-release`, `uname -sr`, `oxctl list` and `ls /` at
  Minimal's console, or waits for the Workstation's greeter (and logs in
  with `--login`), and saves `screenshot.png` in the image directory.
- `hideboot-screenshot [--no-build] [--manager FILE] [--display WxH]`:
  installs Minimal, gives its ESP an entry being tried, a good one, a
  failed one and the recovery system, and saves pictures of hideBoot's
  menu. `--display` sets the screen QEMU announces through EDID.
