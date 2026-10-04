# Applications

The image is the system's; people do not install into it. Graphical
applications come from Flatpak, and command-line and development tools are
meant to live in containers. See
[ARCHITECTURE.md, "Applications and development"](../../ARCHITECTURE.md#applications-and-development).

Everything on this page is in hideOS **Workstation** only. The recipes are
in [recipes/system/apps](../../recipes/system/apps); ROADMAP H5 tracks what
is done.

## Flatpak

[flatpak.toml](../../recipes/system/apps/flatpak.toml), Flatpak 1.18.

- **One system-wide installation**, in `/var/lib/flatpak` on `@var`, which
  members of `wheel` may change without a password (Flatpak's own polkit
  rules), through `flatpak-system-helper` on the system bus.
- **Flathub is preconfigured with nothing in `/etc`.** The recipe installs
  `flathub.flatpakrepo` into `/usr/share/flatpak/remotes.d`, and Flatpak
  applies every file there, with its GPG key, to the system installation
  the first time it opens it.
- **Sandboxes** are the system's bubblewrap and xdg-dbus-proxy, with
  Wayland's security-context protocol, which COSMIC supports. bubblewrap is
  not setuid: it uses unprivileged user namespaces, so the kernel has
  `CONFIG_USER_NS`.
- Applications are kept in libostree's content-addressed store; ostree is
  built only for that, with nothing for booting an ostree system.
- What a system without systemd does not use — Flatpak's systemd units, its
  environment generators, the `profile.d` script — is left out; the COSMIC
  session puts the exported applications in `XDG_DATA_DIRS` itself.

`cargo xtask desktop-test` checks that Flathub is configured, that its
signed summary downloads and verifies, and that bubblewrap makes a sandbox
for an unprivileged user. It does not install an application. See
[[Testing]].

### The launch helper

`flatpak-system-helper` is started on demand by dbus-daemon's setuid launch
helper, which hideOS installs `4755` rather than upstream's `4750
root:messagebus`. Why is in [[Development notes#the-dbus-launch-helper-is-4755]].

## The portals

How a sandboxed application opens a file, takes a screenshot, shares the
screen or sends a notification: by asking the desktop, over D-Bus, instead
of reaching around its sandbox.

- [xdg-desktop-portal](../../recipes/system/apps/xdg-desktop-portal.toml)
  1.22: the desktop-neutral half and the document portal, D-Bus activated
  on the session bus. Icons and sounds that applications hand it are checked
  in bubblewrap before they are shown or played. Built without geoclue and
  gudev, so there is no location or USB portal.
- [xdg-desktop-portal-cosmic](../../recipes/system/apps/xdg-desktop-portal-cosmic.toml)
  (COSMIC epoch 1.9): COSMIC's dialogs — the file chooser, screenshots,
  screen casting through PipeWire, and the settings applications read their
  theme from. xdg-desktop-portal picks it from `cosmic-portals.conf` when
  `XDG_CURRENT_DESKTOP` is `COSMIC`. Built without its systemd feature,
  whose only use is logging to journald; it logs to stderr.

## `hide shell` (planned)

Not written. The plan is a toolbox-style Podman container sharing the
user's home and display, with any distribution inside it, for compilers,
language toolchains and package managers. Podman is not packaged yet. Until
then, the hideOS image has no compiler, by design: images ship without one.
