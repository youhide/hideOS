//! hideOS's default COSMIC theme: Dracula's colours (draculatheme.com, MIT),
//! given to COSMIC's own theme builder, and the theme it builds written as
//! COSMIC's configuration defaults. COSMIC reads the computed theme, not
//! the builder, so both are written: the theme for the desktop, the builder
//! for Settings → Appearance to start from. A person's own choice, in
//! ~/.config/cosmic, comes before these.
//!
//! ```text
//! cargo run --manifest-path tools/cosmic-dracula/Cargo.toml -- \
//!     recipes/system/desktop/desktop-units
//! ```

use std::path::PathBuf;

use cosmic_config::{Config, CosmicConfigEntry};
use cosmic_theme::{DARK_THEME_BUILDER_ID, DARK_THEME_ID, Theme, ThemeBuilder, ThemeMode};
use palette::{Srgb, Srgba};

/// Dracula's palette, from its specification.
const BACKGROUND: u32 = 0x282a36;
const CURRENT_LINE: u32 = 0x44475a;
/// Between Background and Current Line: the containers a window holds.
const CONTAINER: u32 = 0x343746;
const FOREGROUND: u32 = 0xf8f8f2;
const COMMENT: u32 = 0x6272a4;
const GREEN: u32 = 0x50fa7b;
const ORANGE: u32 = 0xffb86c;
const PURPLE: u32 = 0xbd93f9;
const RED: u32 = 0xff5555;

fn rgb(hex: u32) -> Srgb {
    let channel = |shift: u32| ((hex >> shift) & 0xff) as f32 / 255.0;
    Srgb::new(channel(16), channel(8), channel(0))
}

fn rgba(hex: u32) -> Srgba {
    let c = rgb(hex);
    Srgba::new(c.red, c.green, c.blue, 1.0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out: PathBuf = std::env::args()
        .nth(1)
        .ok_or("usage: cosmic-dracula DIR (cosmic/ is written in DIR)")?
        .into();

    let mut builder = ThemeBuilder::dark();
    builder.bg_color = Some(rgba(BACKGROUND));
    builder.primary_container_bg = Some(rgba(CONTAINER));
    builder.secondary_container_bg = Some(rgba(CURRENT_LINE));
    builder.text_tint = Some(rgb(FOREGROUND));
    builder.neutral_tint = Some(rgb(COMMENT));
    builder.accent = Some(rgb(PURPLE));
    builder.success = Some(rgb(GREEN));
    // Dracula's yellow is a text colour, too light for a warning's fill.
    builder.warning = Some(rgb(ORANGE));
    builder.destructive = Some(rgb(RED));
    builder.window_hint = Some(rgb(PURPLE));

    let mut theme: Theme = builder.clone().build();
    theme.name = String::from("Dracula");
    builder.write_entry(&Config::with_custom_path(
        DARK_THEME_BUILDER_ID,
        ThemeBuilder::VERSION,
        out.clone(),
    )?)?;
    theme.write_entry(&Config::with_custom_path(
        DARK_THEME_ID,
        Theme::VERSION,
        out.clone(),
    )?)?;
    // Dark, and staying dark: Dracula has no light half here.
    ThemeMode {
        is_dark: true,
        auto_switch: false,
    }
    .write_entry(&Config::with_custom_path(
        "com.system76.CosmicTheme.Mode",
        ThemeMode::VERSION,
        out.clone(),
    )?)?;
    // Config creates the directory of every earlier version it looks
    // through; only the versions written are kept.
    for name in [DARK_THEME_ID, DARK_THEME_BUILDER_ID] {
        let dir = out.join("cosmic").join(name);
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let _ = std::fs::remove_dir(entry.path());
        }
    }
    println!("wrote {}", out.join("cosmic").display());
    Ok(())
}
