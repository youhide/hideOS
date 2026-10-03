//! Configuration directories in the systemd style that the upstream
//! packages hideOS ships already follow: `*.conf` files in
//! `/usr/lib/<name>.d/`, each replaced by a file of the same name in
//! `/etc/<name>.d/`, read in order of file name.

use std::collections::BTreeMap;

/// Given the file names found in the vendor directory and in the override
/// directory, which file to read for each name, in the order to read them.
/// `true` means the override.
pub fn merge(vendor: &[String], overrides: &[String]) -> Vec<(String, bool)> {
    let mut chosen = BTreeMap::new();
    for name in vendor.iter().filter(|n| n.ends_with(".conf")) {
        chosen.insert(name.clone(), false);
    }
    for name in overrides.iter().filter(|n| n.ends_with(".conf")) {
        chosen.insert(name.clone(), true);
    }
    chosen.into_iter().collect()
}

/// Splits a line into fields on whitespace, keeping a double-quoted field
/// whole. `None` for blank lines and comments.
pub fn fields(line: &str) -> Option<Vec<String>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_field = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_field = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_field {
                    out.push(std::mem::take(&mut current));
                    in_field = false;
                }
            }
            c => {
                current.push(c);
                in_field = true;
            }
        }
    }
    if in_field {
        out.push(current);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_by_name_and_order_is_by_name() {
        let vendor = [
            "b.conf".to_owned(),
            "a.conf".to_owned(),
            "README".to_owned(),
        ];
        let overrides = ["b.conf".to_owned(), "c.conf".to_owned()];
        assert_eq!(
            merge(&vendor, &overrides),
            [
                ("a.conf".to_owned(), false),
                ("b.conf".to_owned(), true),
                ("c.conf".to_owned(), true),
            ]
        );
    }

    #[test]
    fn quoted_fields_stay_whole() {
        assert_eq!(
            fields(r#"u messagebus - "D-Bus daemon" /run/dbus"#).unwrap(),
            ["u", "messagebus", "-", "D-Bus daemon", "/run/dbus"]
        );
        assert_eq!(fields("  # comment"), None);
        assert_eq!(fields(""), None);
        assert_eq!(fields(r#"u x - "" /"#).unwrap(), ["u", "x", "-", "", "/"]);
    }
}
