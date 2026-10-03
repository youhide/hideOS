//! Directories, files and links that have to exist in the writable parts of
//! the system — `/var`, `/run`, `/etc` — declared by packages in
//! `tmpfiles.d` files and created at boot.
//!
//! The format is systemd's: `TYPE PATH MODE USER GROUP AGE ARGUMENT`. The
//! subset: `d` and `D` (a directory; nothing is ever cleaned), `f` (a file,
//! with ARGUMENT as its content if it is new), `L` (a symlink to ARGUMENT).
//! Other types are reported and skipped rather than failing the boot: they
//! clean, relabel or set attributes, and the system works without them.
//! Existing paths are not changed.

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Directory,
    File { content: String },
    Symlink { target: String },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub action: Action,
    pub path: String,
    /// `None`: the default, 0755 for directories and 0644 for files.
    pub mode: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Line(Line),
    /// A type this does not implement, with the line it was on.
    Skipped(String),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

pub fn parse(text: &str) -> Result<Vec<Parsed>, ParseError> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let Some(fields) = crate::config::fields(raw) else {
            continue;
        };
        let error = |message: String| ParseError {
            line: index + 1,
            message,
        };
        let field = |i: usize| fields.get(i).map(String::as_str).filter(|f| *f != "-");
        let kind = field(0).ok_or_else(|| error("no type".to_owned()))?;
        let path = field(1)
            .ok_or_else(|| error("no path".to_owned()))?
            .to_owned();
        // Specifiers (%h, %t, ...) expand per user or per machine; none of
        // the files hideOS ships needs them yet.
        if path.contains('%') || !path.starts_with('/') {
            out.push(Parsed::Skipped(raw.trim().to_owned()));
            continue;
        }
        // `L+`, `d!`: modifiers that change when a line applies. Taken at
        // their plain meaning, which is what they do on a fresh path.
        let base = kind.trim_end_matches(['+', '!', '-', '=', '~', '^']);
        let action = match base {
            "d" | "D" => Action::Directory,
            "f" => Action::File {
                content: field(6).unwrap_or("").to_owned(),
            },
            "L" => Action::Symlink {
                target: field(6)
                    .ok_or_else(|| error(format!("`L {path}` has no target")))?
                    .to_owned(),
            },
            _ => {
                out.push(Parsed::Skipped(raw.trim().to_owned()));
                continue;
            }
        };
        let mode = match field(2) {
            None => None,
            Some(text) => Some(
                u32::from_str_radix(text.trim_start_matches('~'), 8)
                    .map_err(|_| error(format!("`{text}` is not an octal mode")))?,
            ),
        };
        out.push(Parsed::Line(Line {
            action,
            path,
            mode,
            user: field(3).map(str::to_owned),
            group: field(4).map(str::to_owned),
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_dbus_and_friends_ship() {
        let parsed = parse(
            "d /var/lib/dbus 0755 - - -\n\
             L /var/lib/dbus/machine-id - - - - /etc/machine-id\n\
             d /run/greetd 0750 greeter greeter\n\
             f /etc/thing 0600 root root - hello\n\
             x /tmp/keep\n\
             d %t/user 0700\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 6);
        assert_eq!(
            parsed.get(2).unwrap(),
            &Parsed::Line(Line {
                action: Action::Directory,
                path: "/run/greetd".to_owned(),
                mode: Some(0o750),
                user: Some("greeter".to_owned()),
                group: Some("greeter".to_owned()),
            })
        );
        assert!(matches!(
            parsed.get(1).unwrap(),
            Parsed::Line(Line { action: Action::Symlink { target }, .. }) if target == "/etc/machine-id"
        ));
        assert!(matches!(parsed.get(4).unwrap(), Parsed::Skipped(_)));
        assert!(matches!(parsed.get(5).unwrap(), Parsed::Skipped(_)));
    }

    #[test]
    fn bad_modes_and_targetless_links_are_errors() {
        assert!(parse("d /x 0999").is_err());
        assert!(parse("L /x").is_err());
    }
}
