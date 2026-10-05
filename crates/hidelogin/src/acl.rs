//! A device's POSIX ACL, as the kernel keeps it in the
//! `system.posix_acl_access` extended attribute: what gives the person at
//! the screen — and only them — their sound card, camera and security
//! keys, as logind's `uaccess` does.
//!
//! The attribute is a little-endian version (2), then entries of a tag, a
//! permission and an id, ordered by tag and then id.

use thiserror::Error;

const VERSION: u32 = 2;
pub const USER_OBJ: u16 = 0x01;
pub const USER: u16 = 0x02;
pub const GROUP_OBJ: u16 = 0x04;
pub const GROUP: u16 = 0x08;
pub const MASK: u16 = 0x10;
pub const OTHER: u16 = 0x20;
/// Read and write: what a device granted is opened with.
const RW: u16 = 6;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AclError {
    #[error("an ACL of {0} bytes is not a whole number of entries")]
    Length(usize),
    #[error("an ACL of version {0}, not 2")]
    Version(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub tag: u16,
    pub id: u32,
    pub perm: u16,
}

/// The entries the attribute holds.
pub fn parse(bytes: &[u8]) -> Result<Vec<Entry>, AclError> {
    let (header, body) = bytes
        .split_first_chunk::<4>()
        .ok_or(AclError::Length(bytes.len()))?;
    let version = u32::from_le_bytes(*header);
    if version != VERSION {
        return Err(AclError::Version(version));
    }
    if body.len() % 8 != 0 {
        return Err(AclError::Length(bytes.len()));
    }
    let (entries, _) = body.as_chunks::<8>();
    Ok(entries
        .iter()
        .map(|e| {
            let [t0, t1, p0, p1, i0, i1, i2, i3] = *e;
            Entry {
                tag: u16::from_le_bytes([t0, t1]),
                perm: u16::from_le_bytes([p0, p1]),
                id: u32::from_le_bytes([i0, i1, i2, i3]),
            }
        })
        .collect())
}

/// The ACL a file without one has: its mode's three classes.
pub fn from_mode(mode: u32) -> Vec<Entry> {
    // Masked to three bits, so the casts lose nothing.
    let class = |shift: u32| ((mode >> shift) & 7) as u16;
    vec![
        Entry {
            tag: USER_OBJ,
            id: u32::MAX,
            perm: class(6),
        },
        Entry {
            tag: GROUP_OBJ,
            id: u32::MAX,
            perm: class(3),
        },
        Entry {
            tag: OTHER,
            id: u32::MAX,
            perm: class(0),
        },
    ]
}

/// `entries` with read and write for `uid` alone among named users — any
/// other user granted before loses it — or for none with `None`. The mask
/// is what the group and the named entries need.
pub fn grant(entries: &[Entry], uid: Option<u32>) -> Vec<Entry> {
    let mut out: Vec<Entry> = entries
        .iter()
        .copied()
        .filter(|e| e.tag != USER && e.tag != MASK)
        .collect();
    if let Some(uid) = uid {
        out.push(Entry {
            tag: USER,
            id: uid,
            perm: RW,
        });
    }
    let named = out.iter().any(|e| e.tag == USER || e.tag == GROUP);
    if named {
        let perm = out
            .iter()
            .filter(|e| matches!(e.tag, GROUP_OBJ | USER | GROUP))
            .fold(0, |acc, e| acc | e.perm);
        out.push(Entry {
            tag: MASK,
            id: u32::MAX,
            perm,
        });
    }
    out.sort();
    out
}

pub fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut bytes = VERSION.to_le_bytes().to_vec();
    for e in entries {
        bytes.extend_from_slice(&e.tag.to_le_bytes());
        bytes.extend_from_slice(&e.perm.to_le_bytes());
        bytes.extend_from_slice(&e.id.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_card_goes_to_the_active_user_and_back() {
        // crw-rw---- root:audio
        let start = from_mode(0o660);
        let granted = grant(&start, Some(1000));
        assert_eq!(
            granted,
            vec![
                Entry {
                    tag: USER_OBJ,
                    id: u32::MAX,
                    perm: 6
                },
                Entry {
                    tag: USER,
                    id: 1000,
                    perm: 6
                },
                Entry {
                    tag: GROUP_OBJ,
                    id: u32::MAX,
                    perm: 6
                },
                Entry {
                    tag: MASK,
                    id: u32::MAX,
                    perm: 6
                },
                Entry {
                    tag: OTHER,
                    id: u32::MAX,
                    perm: 0
                },
            ]
        );
        let bytes = encode(&granted);
        assert_eq!(bytes.len(), 4 + 5 * 8);
        assert_eq!(parse(&bytes), Ok(granted.clone()));
        // Another person's session comes forward: the first loses it.
        let switched = grant(&granted, Some(1001));
        assert!(switched.iter().any(|e| e.tag == USER && e.id == 1001));
        assert!(!switched.iter().any(|e| e.tag == USER && e.id == 1000));
        // Nobody at the screen: the mode's classes, no mask.
        assert_eq!(grant(&switched, None), start);
    }

    #[test]
    fn what_is_not_an_acl_is_refused() {
        assert_eq!(parse(&[2, 0, 0]), Err(AclError::Length(3)));
        assert_eq!(parse(&[1, 0, 0, 0]), Err(AclError::Version(1)));
        assert_eq!(parse(&[2, 0, 0, 0, 1, 0]), Err(AclError::Length(6)));
    }
}
