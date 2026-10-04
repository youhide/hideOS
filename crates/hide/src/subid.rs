//! Subordinate user and group IDs: the ranges `/etc/subuid` and
//! `/etc/subgid` give each person, which rootless containers map their
//! users into — `hide shell` cannot start without one. `hide setup` gives a
//! range at every boot to each person who has none, so that an account
//! made anywhere — the installer, Settings — has one by its next login.

use std::ops::RangeInclusive;

/// The first ID a range may start at, and each range's size: useradd's.
pub const START: u32 = 100_000;
pub const COUNT: u32 = 65_536;
/// The user IDs of people, as opposed to system users and `nobody`.
pub const PEOPLE: RangeInclusive<u32> = 1000..=59_999;

/// The lines to append to `subid` — the contents of `/etc/subuid` or
/// `/etc/subgid` — so that every person in `passwd` has a range. Each new
/// range is the first block of `COUNT` at or above `START`, aligned to
/// `COUNT`, that overlaps none already there.
pub fn additions(passwd: &str, subid: &str) -> String {
    let mut taken: Vec<(u64, u64)> = Vec::new();
    let mut named: Vec<&str> = Vec::new();
    for line in subid.lines() {
        let mut fields = line.trim().split(':');
        let (Some(name), Some(start), Some(count)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let (Ok(start), Ok(count)) = (start.parse::<u64>(), count.parse::<u64>()) {
            taken.push((start, start.saturating_add(count)));
            named.push(name);
        }
    }
    let mut out = String::new();
    for line in passwd.lines() {
        let mut fields = line.split(':');
        let (Some(name), Some(uid)) = (fields.next(), fields.nth(1)) else {
            continue;
        };
        let Ok(uid) = uid.parse::<u32>() else {
            continue;
        };
        if !PEOPLE.contains(&uid) || named.contains(&name) {
            continue;
        }
        let mut start = u64::from(START);
        let size = u64::from(COUNT);
        while taken.iter().any(|&(a, b)| start < b && a < start + size) {
            start += size;
        }
        taken.push((start, start + size));
        named.push(name);
        out.push_str(&format!("{name}:{start}:{COUNT}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0::/root:/usr/bin/zsh\n\
                          polkitd:x:102:102::/:/usr/bin/nologin\n\
                          youri:x:1000:1000::/home/youri:/usr/bin/zsh\n\
                          ana:x:1001:1001::/home/ana:/usr/bin/zsh\n\
                          nobody:x:65534:65534::/:/usr/bin/nologin\n";

    #[test]
    fn each_person_gets_a_range_in_turn() {
        assert_eq!(
            additions(PASSWD, ""),
            "youri:100000:65536\nana:165536:65536\n"
        );
    }

    #[test]
    fn a_person_with_a_range_keeps_it_and_the_rest_avoid_it() {
        let subid = "youri:100000:65536\n";
        assert_eq!(additions(PASSWD, subid), "ana:165536:65536\n");
        // A range someone set by hand, anywhere, is avoided too.
        let subid = "other:150000:10\n";
        assert_eq!(
            additions(PASSWD, subid),
            "youri:165536:65536\nana:231072:65536\n"
        );
    }

    #[test]
    fn nothing_to_add_when_everyone_has_one() {
        let subid = "youri:100000:65536\nana:165536:65536\n";
        assert_eq!(additions(PASSWD, subid), "");
    }
}
