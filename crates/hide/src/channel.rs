//! Where `hide update` looks when it is given no image: the registry and
//! the channel, from `/usr/lib/hide/update.conf`, each key overridable in
//! `/etc/hide/update.conf`. The image is the registry's tag for this
//! machine's edition and its channel: `ghcr.io/youhide/hideos:workstation-stable`.
//!
//! ```text
//! registry = ghcr.io/youhide/hideos
//! channel = stable
//! ```

pub const CHANNELS: &[&str] = &["stable", "beta", "edge"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub registry: String,
    pub channel: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("update.conf line {0}: expected `key = value`")]
    Syntax(usize),
    #[error("update.conf: unknown key `{0}`")]
    Key(String),
    #[error("update.conf: `{0}` is not a channel; channels are stable, beta and edge")]
    Channel(String),
    #[error("update.conf names no {0}")]
    Missing(&'static str),
}

/// The settings from the vendor file, then the override over it, key by key.
pub fn read(vendor: &str, overrides: &str) -> Result<Settings, ChannelError> {
    let mut registry = None;
    let mut channel = None;
    for text in [vendor, overrides] {
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(ChannelError::Syntax(n + 1))?;
            let value = value.trim().to_owned();
            match key.trim() {
                "registry" => registry = Some(value),
                "channel" => channel = Some(value),
                other => return Err(ChannelError::Key(other.to_owned())),
            }
        }
    }
    let registry = registry.ok_or(ChannelError::Missing("registry"))?;
    let channel = channel.ok_or(ChannelError::Missing("channel"))?;
    if !CHANNELS.contains(&channel.as_str()) {
        return Err(ChannelError::Channel(channel));
    }
    Ok(Settings { registry, channel })
}

impl Settings {
    /// The image for `edition` on this channel.
    pub fn image(&self, edition: &str) -> String {
        format!("{}:{edition}-{}", self.registry, self.channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VENDOR: &str = "# hideOS\nregistry = ghcr.io/youhide/hideos\nchannel = stable\n";

    #[test]
    fn the_vendor_file_alone() {
        let settings = read(VENDOR, "").unwrap();
        assert_eq!(
            settings.image("workstation"),
            "ghcr.io/youhide/hideos:workstation-stable"
        );
    }

    #[test]
    fn an_override_changes_one_key() {
        let settings = read(VENDOR, "channel = edge\n").unwrap();
        assert_eq!(
            settings.image("minimal"),
            "ghcr.io/youhide/hideos:minimal-edge"
        );
        assert_eq!(settings.registry, "ghcr.io/youhide/hideos");
    }

    #[test]
    fn what_is_refused() {
        assert_eq!(
            read(VENDOR, "channel = nightly"),
            Err(ChannelError::Channel("nightly".into()))
        );
        assert_eq!(
            read(VENDOR, "mirror = x"),
            Err(ChannelError::Key("mirror".into()))
        );
        assert_eq!(read(VENDOR, "registry"), Err(ChannelError::Syntax(1)));
        assert_eq!(
            read("channel = stable", ""),
            Err(ChannelError::Missing("registry"))
        );
    }
}
