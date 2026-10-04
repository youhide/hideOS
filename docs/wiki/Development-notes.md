# Development notes

Things this tree learned the hard way, one entry each, with the reason.
Add an entry in the commit that learned it; remove one when the code it
describes goes away. Newest last.

## One image build at a time

Never run two `cargo xtask image` builds — or two tests, which each build
first — at once. Both run hideforge in the builder against the same `/work`
volume, and hideforge takes no lock: two runs that build the same recipe
share its scratch directory `/work/build/<hash>` and its log, and the
image staging directory, and collide.

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
