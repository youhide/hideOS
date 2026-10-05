# Development notes

Things this tree learned the hard way, one entry each, with the reason.
Add an entry in the commit that learned it; remove one when the code it
describes goes away. Newest last.

## One image build at a time, per checkout

Two tests in one checkout at once collide: each owns the image directory
in `target/images` — its disks, the firmware's variables, the image it
assembles there. Side by side, tests run through `cargo xtask round`
(see [[Testing#a-round-of-tests]]), each lane in a copy of the checkout.

Lanes share the builder's `/work`, and hideforge is made for it: a lock
per recipe and per image (`Layout::lock`), so the second run to want a
recipe finds it in the store, and scratch names carrying the container's
host name as well as the PID (`run_id`) — in the builder every run is
PID 7, and two of them once shared `/work/build/.hideforge-7` ("Text file
busy").

One thing the lanes must not share is a changed hideforge: cargo in the
builder builds it into `/work/target`, from whichever lane's `/src` asks,
so lanes with different hideforge sources rebuild it over each other.
`round` copies the checkout to every lane at its start; change hideforge
between rounds, not during one.

## Firmware variables persist per disk

QEMU's UEFI variables are kept in the image directory, `efivars.fd` (and
`efivars-secure.fd` under Secure Boot), from one boot to the next, as a
machine keeps its NVRAM, because hibernation and the boot manager leave
variables for the next boot: `hide swap`, for one, records in a variable
where a hibernated system is to be found, and the next boot's hidestage
reads it. A fresh disk gets fresh variables (`fresh_firmware_variables` in
`crates/xtask/src/main.rs`), because a new disk is a new machine.

## QEMU's `bootindex` rewrites BootOrder

A `bootindex=` on a QEMU device is passed to the firmware, which writes it
into its `BootOrder` variable — and that can leave the installed disk out
of the next boot. A real machine's boot menu, choosing a USB stick once,
writes nothing. So `installer-test`, after booting the installer medium by
its boot index to reinstall, resets the firmware variables before booting
the disk.

## Firmware lists display modes the display does not have

OVMF's QEMU video driver lists modes up to 4096x2160 whatever the display
is, and a mode above the panel's is a blank or scaled screen. So hideBoot
(0.2.0) draws its menu at the display's native resolution, read from its
EDID, and otherwise stays in the mode the firmware chose; never simply the
largest mode. `cargo xtask hideboot-screenshot --display WxH` sets the
EDID QEMU announces.

## Find an image by its manifest digest after a pull

composefs-oci, when it pulls an image, rewrites the manifest's splitstream
to name the EROFS image it made. The manifest verity the pull returns is
the one from before that rewrite, so looking the EROFS image up with it
fails. Look it up by manifest digest alone. (Fixed in `hide ext add` and
hideforge's payload by commit `eb1e701`.)

## NetworkManager needs `GETTEXTDATADIRS` and `PYTHONDONTWRITEBYTECODE`

`msgfmt` merges translations into polkit policy files using the ITS rules
polkit installs, in `/usr/share/gettext`, where the stage-1 gettext does
not look by itself: the recipe sets `GETTEXTDATADIRS`. And `gdbus-codegen`
is Python from glib: left alone it writes its bytecode cache into glib's
directory, a file the recipe would then ship — and so would the next
recipe that runs the same generator, which hideforge refuses as two outputs
providing one path. `PYTHONDONTWRITEBYTECODE=1` is set in every recipe that
runs glib's Python tools.

## `hide install` reads memory at install time

`hide install` runs as PID 1 of a QEMU guest, where `/proc` is not mounted
yet while the arguments are parsed. So the swap file's default size — the
machine's memory, from `/proc/meminfo` — is read when the install runs,
after `/proc` is mounted, not when `--swap` is parsed.

## The screen is the console; tests append the serial port

Every UKI hideforge makes ends its command line with `console=tty0`, so
`/dev/console` — where hidestage asks for the passphrase, the installer
asks its questions and Minimal's shell runs — is the screen. The kernel
writes to the serial port too. Test builds (everything `cargo xtask`
builds) append `console=ttyS0`, which makes the serial port the console, so
the tests can read and type on it.

## The dbus launch helper is `4755`

dbus-daemon's launch helper starts system services on demand — Flatpak's
system helper. Upstream installs it `4750 root:messagebus`, but the
`messagebus` group is created at boot by `hide setup`, after the image is
sealed, so the image cannot carry that group on the file. It is installed
`4755` instead; the helper itself refuses any caller but the bus's own
user.

## A rename on FAT is not durable by itself

`update-test`, cutting the power after each step of an update, found a
zero-length UKI on the ESP. A rename on FAT can reach the disk before the
file's data does. Renames on the ESP now sync the file under its new name,
its directory and the filesystem (`rename_durably` in
`crates/hide/src/deploy.rs`).

## Stopping `cargo xtask` leaves the builder running

`cargo xtask` runs hideforge in a docker container, and killing xtask does
not stop it: the build goes on in the background, holding `/work`. A new
build started then waits on its locks, or, before they existed,
collided with it — "Text file busy" on hideforge's own binary was the
sign. Stop it with
`docker ps -q | xargs docker kill` before starting another.

## `SOURCE_DATE_EPOCH` counts unpacked files only

hideforge sets `SOURCE_DATE_EPOCH` to the newest file the sources unpack
to. It used to count every directory entry, including the directories it
makes for a source's `dest` and sources copied in whole (`extract =
false`) — both made at build time, so the epoch was the time of the build
and the kernel's build timestamp changed every time. A copied source is now
given the epoch instead (`newest_mtime` in `crates/hideforge/src/fetch.rs`).

## The kernel's modules are indexed after they are installed

`make modules_install` runs depmod with `-b $INSTALL_MOD_PATH`, and the
kernel installs to `$INSTALL_MOD_PATH/lib/modules`. With `/usr` for the
first, kmod — built with `/usr/lib/modules` as its module directory —
looks in `/usr/usr/lib/modules`. `linux.toml` installs with `DEPMOD=true`
and runs `depmod` itself.

## Daemons that leave their cgroup

elogind, as a cgroup controller, moves itself out of the cgroup oxinit
started it in. Signalling the cgroup then reached nothing, and the
Workstation never powered off. oxinit now also signals the process it
forked when it is outside its cgroup, and wakes for its shutdown deadline
(youhide/oxinit#25, released in v0.2.0).
