# cosmic-dracula

hideOS's default COSMIC theme is [Dracula](https://draculatheme.com), by
Zeno Rocha and contributors, MIT licensed. This program gives Dracula's
colours to COSMIC's own theme builder (libcosmic's `cosmic-theme`, at the
commit COSMIC 1.9 is built with) and writes what it builds as COSMIC
configuration defaults:

```sh
cargo run --manifest-path tools/cosmic-dracula/Cargo.toml -- recipes/system/desktop/desktop-units
```

The output lands in `recipes/system/desktop/desktop-units/cosmic/`, which the
`desktop-units` recipe installs in `/usr/share/hideos/cosmic`, first in
`XDG_DATA_DIRS`: a default, read when a person has chosen nothing.
Settings → Appearance changes it, in `~/.config/cosmic`.

Run it again when COSMIC is updated: the theme is computed, and a new
libcosmic may compute it differently or version its format. Change the `rev`
in Cargo.toml to the libcosmic commit in the new cosmic-settings'
Cargo.lock first.

The terminal's colours are not computed: `cosmic/com.system76.CosmicTerm/v1`
holds Dracula's ANSI palette, from its specification, as a cosmic-term
colour scheme, and makes it the dark one. Written by hand, since cosmic-term
reads it as it is.
