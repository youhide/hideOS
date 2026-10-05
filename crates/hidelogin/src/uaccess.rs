//! Which devices the person at the screen gets, and who that is, from the
//! files udev, the kernel and hidelogin keep: udev's tag directory lists
//! the devices the `uaccess` rule tagged, by type and number (`c116:5`),
//! the kernel's sysfs names each one's node, and hidelogin's seat file
//! names the active session's user.

/// Where udev lists the devices a tag was given, one file per device.
pub const TAGGED: &str = "/run/udev/tags/uaccess";

/// Where sysfs describes a device udev lists as `c116:5` or `b11:0`: its
/// `uevent` names the node. None for an entry that is not a device number.
pub fn sysfs_uevent(tagged: &str) -> Option<String> {
    let (kind, number) = tagged.split_at_checked(1)?;
    let kind = match kind {
        "c" => "char",
        "b" => "block",
        _ => return None,
    };
    let (major, minor) = number.split_once(':')?;
    if major.parse::<u32>().is_err() || minor.parse::<u32>().is_err() {
        return None;
    }
    Some(format!("/sys/dev/{kind}/{major}:{minor}/uevent"))
}

/// The device node a `uevent` names (`DEVNAME=snd/controlC0`), under /dev.
pub fn devnode(uevent: &str) -> Option<String> {
    uevent
        .lines()
        .find_map(|l| l.strip_prefix("DEVNAME="))
        .filter(|n| !n.is_empty() && !n.split('/').any(|part| part == ".."))
        .map(|n| format!("/dev/{n}"))
}

/// The active session's user, from hidelogin's seat file; none when the
/// screen has nobody.
pub fn active_uid(seat: &str) -> Option<u32> {
    seat.lines()
        .find_map(|l| l.strip_prefix("ACTIVE_UID="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_and_the_active_user() {
        assert_eq!(
            sysfs_uevent("c116:5"),
            Some("/sys/dev/char/116:5/uevent".into())
        );
        assert_eq!(
            sysfs_uevent("b11:0"),
            Some("/sys/dev/block/11:0/uevent".into())
        );
        assert_eq!(sysfs_uevent("+sound:card0"), None);
        assert_eq!(sysfs_uevent("c../:1"), None);
        assert_eq!(
            devnode("MAJOR=116\nMINOR=5\nDEVNAME=snd/controlC0\n"),
            Some("/dev/snd/controlC0".into())
        );
        assert_eq!(devnode("MAJOR=116\n"), None);
        assert_eq!(devnode("DEVNAME=../etc/shadow\n"), None);
        assert_eq!(
            active_uid("IS_SEAT0=1\nACTIVE=2\nACTIVE_UID=1000\n"),
            Some(1000)
        );
        assert_eq!(active_uid("ACTIVE=\nACTIVE_UID=\n"), None);
    }
}
