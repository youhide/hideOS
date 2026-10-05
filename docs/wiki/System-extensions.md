# System extensions

A system extension adds files to `/usr` without changing the sealed image:
a driver the base image should not carry (NVIDIA's is the first), a
security fix ahead of the next image, an optional feature. It is the
analogue of a macOS Cryptex, and **only hideOS publishes them**: a machine
merges an extension only when hideOS signed it, and only over the system
image it was built for. The design is
[ARCHITECTURE.md, "System extensions"](../../ARCHITECTURE.md#system-extensions);
the status is ROADMAP H6.

## The format

An extension is a composefs image in the same store as the system: every
file's content is an object checked by fs-verity, shared with the system
where it is the same file. It travels as an OCI image, like an update:

- **one layer**, the files, all under `/usr`, including
  `/usr/lib/extension-release.d/extension-release.NAME`, which says
  `ID=hideos` and `HIDEOS_IMAGE=sha256:<digest>`: the system image it is
  for;
- **three annotations on the manifest**, which the image digest does not
  cover: `os.hide.extension.name`; `os.hide.extension.signature`, hex; and
  `os.hide.extension.for`, the system image it was built for, which `hide`
  records without mounting it (hidestage still checks the signed
  extension-release).

Why composefs rather than an EROFS image under dm-verity, as systemd's
extensions are: hideOS already has a verified store, and an extension in it
is mounted by the code that mounts the system — no loop devices, no second
integrity mechanism.

## Signing

The signature is over the text `sha256:<the extension's composefs image
digest>`, with RSA, PKCS #1 v1.5 and SHA-256: the key and scheme the UKI is
signed with, so one key stands for "made by hideOS". Today that is the
development key; see [[Boot chain]]. Verification is hidecrypt's
([crates/hidecrypt/src/signature.rs](../../crates/hidecrypt/src/signature.rs));
signing is the build's, with openssl.

## Building one

```sh
cargo xtask forge -- sysext NAME --image DIGEST --output DIR --sign /work/keys/dev
```

hideforge builds the recipe `NAME`, which must install under `/usr` only,
adds the extension-release file naming `DIGEST` (the system image, from its
`image.digest`), and writes an OCI archive with the annotations, signed
with `DIR/db.key` when `--sign` is given. The test extension is
[recipes/system/test/hello-sysext.toml](../../recipes/system/test/hello-sysext.toml):
one command, `hello-sysext`.

## `hide ext`

```sh
hide ext add oci-archive:PATH     # or oci:DIR[:TAG]
hide ext list
hide ext remove NAME
```

([crates/hide/src/ext.rs](../../crates/hide/src/ext.rs))

- `add` pulls the image into the store, checks the signature now rather
  than at the next boot, refusing an unsigned or wrongly signed one, tags
  the image in the store as `extension-NAME-<system>` so that garbage
  collection keeps it, and adds it to the record hidestage reads,
  `/hideos/extensions/NAME` — one entry for each system image the machine
  has a build for, so that going back to the previous system merges the
  previous build:

  ```text
  image=sha256:<the extension's image>
  signature=<hex>
  for=sha256:<the system image>

  image=…
  ```

  The format is `hidestage::extension`'s
  ([crates/hidestage/src/extension.rs](../../crates/hidestage/src/extension.rs)),
  shared by `hide` and hidestage.

- `list` names each recorded extension, whether it is merged in the
  running system, and how many builds the machine has.
- `remove` deletes the record and the tag; the extension is gone from the
  next boot, and its objects at the next collection.

`add` and `remove` go through hideupd when it runs (`AddExtension`,
`RemoveExtension`, polkit action `os.hide.update.extensions`); see
[[Updates and rollback]].

## At boot

hidestage, after it has mounted the system and before it binds the
writable subvolumes, takes each record in `/hideos/extensions` and merges
the extension only when all three hold
([crates/hidestage/src/sysext.rs](../../crates/hidestage/src/sysext.rs)):

1. hideOS signed its image digest — checked with the public key compiled
   into hidestage, which is trusted because the firmware checked the UKI it
   is in;
2. the image measures to that digest;
3. its extension-release names the system image that is booting.

The extensions that pass are mounted under `/run/hidestage/extensions/` and
stacked over `/usr` with overlayfs, with no upper directory, so `/usr`
stays read-only. One that fails any check is left out with a line on the
console (`hidestage: extension NAME left out: …`); an extension never stops
a boot.

The record's entry for the booting system is the one tried; a record from
before entries named their system has one, tried on any system as before.

`cargo xtask sysext-test` checks the four cases; see [[Testing]].

## Updates bring them

An extension is built for one system image, so an update needs the
extension's build for the image it brings. hideOS publishes each image's
extensions beside it, in the same repository, as `ext-<name>-<image
digest>`, before the channel's tag moves to the image (`cargo xtask
publish`). `hide update` pulls the new image, then fetches the build of
every extension the machine has for it, and commits only when it has them
all; when one is not there it stops — `the update waits: there is no build
of the nvidia extension for this system yet` — and the machine stays as it
was. Garbage collection drops the builds for systems no longer on the disk.

`cargo xtask registry-test` updates a machine with the test extension:
the update waits while the registry has no build for the new image, brings
it once there is one, and after a rollback the old build is merged again.

## NVIDIA

[recipes/system/nvidia/nvidia.toml](../../recipes/system/nvidia/nvidia.toml),
for the Workstation, published with each of its images:

- the open GPU kernel modules (Turing — RTX 20, GTX 16 — and newer), built
  against the build tree of the image's kernel, with the module indexes
  made over the image's modules and these — which is why the kernel's
  indexes are a package of their own, `linux-module-index`, which the
  extension's shadow;
- NVIDIA's userspace from the `.run`, extracted and never run, installed
  byte for byte as its licence requires: EGL and GLES behind libglvnd (Mesa
  is built behind libglvnd too, so both vendors live in one process), the
  GBM backend, CUDA, NVML and `nvidia-smi`, the GSP firmware;
- the EGL platform libraries for Wayland and GBM, and `nvidia-modprobe`,
  built from NVIDIA's sources;
- modprobe.d — nouveau kept from binding, nvidia-drm loaded with nvidia,
  modesetting on — and a udev rule that makes the device files.

No GLX, Vulkan ICD or VDPAU (hideOS has no X11 and no Vulkan loader yet).
Flatpak applications are unaffected: they get NVIDIA's userspace from
Flathub's GL extension.

`cargo xtask nvidia-test` adds it to a Workstation in QEMU, which has no
NVIDIA GPU, and checks what can be checked without one: it merges; its
modules are the running kernel's and are indexed; `nvidia.ko` loads as far
as finding no GPU; every library links; and the desktop still comes up,
drawn by Mesa. A real NVIDIA machine is the rest of the check.
