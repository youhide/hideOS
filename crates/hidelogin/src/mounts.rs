//! What is mounted inside a directory, from `/proc/self/mountinfo`: a
//! user's runtime directory holds FUSE mounts — the document portal's
//! `doc` — that have to be detached before the directory can go.

/// The mount points at or under `dir`, deepest first, so that each is
/// detached before the one it sits on. Mountinfo escapes a space, tab,
/// newline and backslash in a path as three octal digits.
pub fn mounts_under(mountinfo: &str, dir: &str) -> Vec<String> {
    let dir = dir.trim_end_matches('/');
    let mut found: Vec<String> = mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(unescape)
        .filter(|point| {
            point == dir
                || point
                    .strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .collect();
    found.sort_by_key(|point| std::cmp::Reverse(point.matches('/').count()));
    found.dedup();
    found
}

fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|digits| byte == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)))
            .and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok());
        match octal {
            Some(value) => {
                out.push(value);
                i += 4;
            }
            None => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
22 1 0:20 / / rw - composefs composefs ro
40 22 0:30 / /run rw - tmpfs tmpfs rw
81 40 0:44 / /run/user/1000 rw - tmpfs tmpfs rw
90 81 0:50 / /run/user/1000/doc rw - fuse.portal portal rw
91 81 0:51 / /run/user/1000/gvfs\\040data rw - fuse gvfsd rw
92 40 0:52 / /run/user/10000 rw - tmpfs tmpfs rw
";

    #[test]
    fn the_runtime_directorys_mounts_deepest_first() {
        assert_eq!(
            mounts_under(MOUNTINFO, "/run/user/1000/"),
            [
                "/run/user/1000/doc",
                "/run/user/1000/gvfs data",
                "/run/user/1000"
            ]
        );
        // A directory whose name only begins the same is not inside.
        assert_eq!(
            mounts_under(MOUNTINFO, "/run/user/100"),
            Vec::<String>::new()
        );
    }
}
