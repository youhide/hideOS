# System extensions

A system extension adds files to `/usr` without changing the sealed image:
a driver the base image should not carry (NVIDIA is the first planned), a
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
- **two annotations on the manifest**, which the image digest does not
  cover: `os.hide.extension.name`, and `os.hide.extension.signature`, hex.

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
  the image in the store as `extension-NAME` so that garbage collection
  keeps it, and writes the record hidestage reads,
  `/hideos/extensions/NAME`:

  ```text
  image=sha256:<hex>
  signature=<hex>
  ```

- `list` names each recorded extension and whether it is merged in the
  running system.
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

An extension built for one image is not merged into the next one after an
update: it has to be rebuilt for the new image and added again.

`cargo xtask sysext-test` checks the four cases; see [[Testing]].
