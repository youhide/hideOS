//! A system extension's record, `/hideos/extensions/<name>`: which images
//! of the extension the machine has, one for each system image it was
//! built for. `hide ext add` and `hide update` write it, hidestage reads it
//! at boot and merges the one built for the system that is booting.
//!
//! One entry per system, not one per extension, because an extension is
//! built for exactly one system image and a machine keeps more than one: an
//! update fetches the extension's build for the new system beside the one
//! for the running system, which a rollback still needs.
//!
//! ```text
//! image=sha256:<the extension's composefs image>
//! signature=<hex>
//! for=sha256:<the system image it was built for>
//!
//! image=…
//! ```
//!
//! Entries are separated by a blank line. `for=` is not signed: it says
//! which entry to try, and hidestage still checks the extension-release
//! inside the signed image. A record written before there was `for=` has
//! one entry without it, which is tried on any system as before.

/// One build of an extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `sha256:<hex>`: the extension's composefs image digest, which the
    /// signature is over.
    pub image: String,
    /// hideOS's signature over `image`, hex.
    pub signature: String,
    /// `sha256:<hex>`: the system image it was built for, when known.
    pub built_for: Option<String>,
}

/// The entries of a record, in order; an entry missing its image or its
/// signature is left out.
pub fn parse(record: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut image = None;
    let mut signature = None;
    let mut built_for = None;
    let mut finish = |image: &mut Option<String>,
                      signature: &mut Option<String>,
                      built_for: &mut Option<String>| {
        if let (Some(image), Some(signature)) = (image.take(), signature.take()) {
            entries.push(Entry {
                image,
                signature,
                built_for: built_for.take(),
            });
        }
        *built_for = None;
    };
    for line in record.lines().map(str::trim) {
        if line.is_empty() {
            finish(&mut image, &mut signature, &mut built_for);
        } else if let Some(value) = line.strip_prefix("image=") {
            image = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("signature=") {
            signature = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("for=") {
            built_for = Some(value.to_owned());
        }
    }
    finish(&mut image, &mut signature, &mut built_for);
    entries
}

/// The record's text for `entries`.
pub fn render(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|entry| {
            let mut text = format!("image={}\nsignature={}\n", entry.image, entry.signature);
            if let Some(system) = &entry.built_for {
                text.push_str(&format!("for={system}\n"));
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The entry to merge over `system` (`sha256:<hex>`): the one built for
/// it, or else one that does not say what it was built for.
pub fn for_system<'a>(entries: &'a [Entry], system: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .find(|e| e.built_for.as_deref() == Some(system))
        .or_else(|| entries.iter().find(|e| e.built_for.is_none()))
}

/// `entries` with `entry` in: it replaces the one built for the same
/// system, or every entry when it does not say what it was built for.
pub fn with(entries: &[Entry], entry: Entry) -> Vec<Entry> {
    let mut kept: Vec<Entry> = match &entry.built_for {
        Some(system) => entries
            .iter()
            .filter(|e| e.built_for.as_deref() != Some(system.as_str()))
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    kept.push(entry);
    kept
}

/// `entries` without the builds for systems no longer on the machine:
/// those built for a system `kept` says yes to, and those that do not say
/// what they were built for.
pub fn keeping(entries: &[Entry], kept: impl Fn(&str) -> bool) -> Vec<Entry> {
    entries
        .iter()
        .filter(|e| e.built_for.as_deref().is_none_or(&kept))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(image: &str, system: Option<&str>) -> Entry {
        Entry {
            image: format!("sha256:{image}"),
            signature: "ab".into(),
            built_for: system.map(|s| format!("sha256:{s}")),
        }
    }

    #[test]
    fn a_record_from_before_for_is_one_entry_for_any_system() {
        let entries = parse("image=sha256:e1\nsignature=ab\n");
        assert_eq!(entries, [entry("e1", None)]);
        assert_eq!(for_system(&entries, "sha256:s9"), Some(&entry("e1", None)));
    }

    #[test]
    fn the_entry_for_the_booting_system_is_chosen() {
        let entries = vec![entry("e1", Some("s1")), entry("e2", Some("s2"))];
        let text = render(&entries);
        assert_eq!(parse(&text), entries);
        assert_eq!(
            for_system(&entries, "sha256:s2"),
            Some(&entry("e2", Some("s2")))
        );
        assert_eq!(for_system(&entries, "sha256:s3"), None);
    }

    #[test]
    fn a_new_build_replaces_the_one_for_its_system_only() {
        let entries = vec![entry("e1", Some("s1")), entry("e2", Some("s2"))];
        let replaced = with(&entries, entry("e3", Some("s2")));
        assert_eq!(replaced, [entry("e1", Some("s1")), entry("e3", Some("s2"))]);
        let manual = with(&replaced, entry("e4", None));
        assert_eq!(manual, [entry("e4", None)]);
    }

    #[test]
    fn builds_for_systems_gone_are_dropped() {
        let entries = vec![
            entry("e1", Some("s1")),
            entry("e2", Some("s2")),
            entry("e0", None),
        ];
        let kept = keeping(&entries, |system| system == "sha256:s2");
        assert_eq!(kept, [entry("e2", Some("s2")), entry("e0", None)]);
    }

    #[test]
    fn an_incomplete_entry_is_left_out() {
        assert!(parse("image=sha256:e1\n\nsignature=ab\n").is_empty());
    }
}
