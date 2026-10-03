//! Deployments as the ESP holds them: one UKI each, in `EFI/Linux`, named
//!
//! ```text
//! hideos-EDITION-VERSION-DIGEST12[+LEFT[-DONE]].efi
//! ```
//!
//! The suffix is systemd-boot's boot counting (its "Automatic Boot
//! Assessment"): a UKI named `…+3.efi` gets three attempts; systemd-boot
//! renames it `…+2-1.efi` before booting it, and so on; at `+0-3` it sorts
//! after every good entry, so the previous deployment boots instead. A name
//! without the suffix is a deployment that has booted to completion —
//! `hide boot-ok` removes the suffix. Until hideBoot (H7), which keeps the
//! same contract.

use std::cmp::Ordering;

/// The attempts a new deployment gets before the machine goes back.
pub const TRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uki {
    pub edition: String,
    pub version: u64,
    /// The first twelve hex digits of the image's fs-verity digest.
    pub digest: String,
    pub counter: Option<Counter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counter {
    pub left: u32,
    pub done: u32,
}

impl Uki {
    /// Parses a file name in `EFI/Linux`. `None` for anything that is not a
    /// hideOS UKI.
    pub fn parse(file_name: &str) -> Option<Uki> {
        let stem = file_name.strip_suffix(".efi")?;
        let (name, counter) = match stem.rsplit_once('+') {
            Some((name, count)) => {
                let (left, done) = match count.split_once('-') {
                    Some((left, done)) => (left.parse().ok()?, done.parse().ok()?),
                    None => (count.parse().ok()?, 0),
                };
                (name, Some(Counter { left, done }))
            }
            None => (stem, None),
        };
        let rest = name.strip_prefix("hideos-")?;
        let (rest, digest) = rest.rsplit_once('-')?;
        let (edition, version) = rest.rsplit_once('-')?;
        if digest.len() != 12 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        if edition.is_empty() {
            return None;
        }
        Some(Uki {
            edition: edition.to_owned(),
            version: version.parse().ok()?,
            digest: digest.to_owned(),
            counter,
        })
    }

    /// The name without boot counting: the deployment's identity.
    pub fn base_name(&self) -> String {
        format!("hideos-{}-{}-{}", self.edition, self.version, self.digest)
    }

    pub fn file_name(&self) -> String {
        match self.counter {
            None => format!("{}.efi", self.base_name()),
            Some(Counter { left, done: 0 }) => format!("{}+{left}.efi", self.base_name()),
            Some(Counter { left, done }) => format!("{}+{left}-{done}.efi", self.base_name()),
        }
    }

    /// A new deployment, with its attempts.
    pub fn new_deployment(edition: &str, version: u64, digest: &str) -> Uki {
        Uki {
            edition: edition.to_owned(),
            version,
            digest: digest.get(..12).unwrap_or(digest).to_owned(),
            counter: Some(Counter {
                left: TRIES,
                done: 0,
            }),
        }
    }

    /// Booted to completion: no counter.
    pub fn good(&self) -> Uki {
        Uki {
            counter: None,
            ..self.clone()
        }
    }

    /// Out of attempts: systemd-boot will not choose it while anything
    /// else can boot.
    pub fn bad(&self) -> Uki {
        let done = self.counter.map(|c| c.done + c.left).unwrap_or(TRIES);
        Uki {
            counter: Some(Counter { left: 0, done }),
            ..self.clone()
        }
    }

    pub fn is_bad(&self) -> bool {
        matches!(self.counter, Some(Counter { left: 0, .. }))
    }

    pub fn state(&self) -> &'static str {
        match self.counter {
            None => "good",
            Some(Counter { left: 0, .. }) => "bad",
            Some(_) => "trying",
        }
    }
}

/// The order systemd-boot boots in, first first: usable before exhausted,
/// then newest version first. (systemd-boot compares versions in the name
/// the same way: hideOS versions are plain integers.)
pub fn boot_order(ukis: &mut [Uki]) {
    ukis.sort_by(|a, b| match (a.is_bad(), b.is_bad()) {
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        _ => b
            .version
            .cmp(&a.version)
            .then_with(|| b.digest.cmp(&a.digest)),
    });
}

/// Which deployments an update keeps on the ESP: the one running, the
/// newest good one other than it (the way back), and the new one. Everything
/// else may go.
pub fn keep<'a>(ukis: &'a [Uki], booted: &str, new: &str) -> Vec<&'a Uki> {
    let mut kept: Vec<&Uki> = ukis
        .iter()
        .filter(|u| u.digest == booted || u.digest == new)
        .collect();
    if let Some(fallback) = ukis
        .iter()
        .filter(|u| u.digest != booted && u.digest != new && u.counter.is_none())
        .max_by_key(|u| u.version)
    {
        kept.push(fallback);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "a1769de2c221";

    #[test]
    fn names_round_trip_with_and_without_counting() {
        for name in [
            "hideos-minimal-12-a1769de2c221.efi",
            "hideos-minimal-12-a1769de2c221+3.efi",
            "hideos-minimal-12-a1769de2c221+2-1.efi",
            "hideos-workstation-130-a1769de2c221+0-3.efi",
        ] {
            assert_eq!(Uki::parse(name).unwrap().file_name(), name);
        }
        let uki = Uki::parse("hideos-minimal-12-a1769de2c221+2-1.efi").unwrap();
        assert_eq!(uki.edition, "minimal");
        assert_eq!(uki.version, 12);
        assert_eq!(uki.digest, D);
        assert_eq!(uki.counter, Some(Counter { left: 2, done: 1 }));
        assert_eq!(uki.state(), "trying");
    }

    #[test]
    fn other_files_are_not_deployments() {
        for name in [
            "BOOTX64.EFI",
            "hideos-minimal-a1769de2c221.efi",
            "hideos-minimal-x-a1769de2c221.efi",
            "hideos-minimal-12-a1769de2.efi",
            "hideos-minimal-12-a1769de2c221+x.efi",
            "other-minimal-12-a1769de2c221.efi",
        ] {
            assert_eq!(Uki::parse(name), None, "{name}");
        }
    }

    #[test]
    fn good_and_bad_rename_only_the_counter() {
        let uki = Uki::new_deployment("minimal", 13, "b2c3d4e5f6a7b8c9");
        assert_eq!(uki.file_name(), "hideos-minimal-13-b2c3d4e5f6a7+3.efi");
        assert_eq!(uki.good().file_name(), "hideos-minimal-13-b2c3d4e5f6a7.efi");
        assert_eq!(
            uki.bad().file_name(),
            "hideos-minimal-13-b2c3d4e5f6a7+0-3.efi"
        );
        let tried = Uki::parse("hideos-minimal-13-b2c3d4e5f6a7+1-2.efi").unwrap();
        assert_eq!(
            tried.bad().file_name(),
            "hideos-minimal-13-b2c3d4e5f6a7+0-3.efi"
        );
    }

    #[test]
    fn newest_usable_boots_first_and_exhausted_last() {
        let mut ukis: Vec<Uki> = [
            "hideos-minimal-12-a1769de2c221.efi",
            "hideos-minimal-14-cccccccccccc+0-3.efi",
            "hideos-minimal-13-bbbbbbbbbbbb+2-1.efi",
        ]
        .iter()
        .map(|n| Uki::parse(n).unwrap())
        .collect();
        boot_order(&mut ukis);
        let versions: Vec<u64> = ukis.iter().map(|u| u.version).collect();
        assert_eq!(versions, [13, 12, 14]);
    }

    #[test]
    fn an_update_keeps_the_running_the_way_back_and_the_new() {
        let ukis: Vec<Uki> = [
            "hideos-minimal-10-aaaaaaaaaaaa.efi",
            "hideos-minimal-11-bbbbbbbbbbbb.efi",
            "hideos-minimal-12-cccccccccccc.efi",
            "hideos-minimal-13-dddddddddddd+0-3.efi",
        ]
        .iter()
        .map(|n| Uki::parse(n).unwrap())
        .collect();
        let kept: Vec<u64> = keep(&ukis, "cccccccccccc", "eeeeeeeeeeee")
            .iter()
            .map(|u| u.version)
            .collect();
        assert_eq!(kept, [12, 11]);
    }
}
