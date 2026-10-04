//! The first person's account, made by the installer: the macOS setup
//! assistant's job. Everyone after the first is added from Settings.

use sha_crypt::{PasswordHasher, ShaCrypt};

/// The first regular user's ID; system users are below it.
pub const FIRST_UID: u32 = 1000;
/// Groups the first user joins: `wheel`, which is who may administer the
/// machine, as the first account on a Mac is an administrator.
pub const ADMIN_GROUPS: &[&str] = &["wheel"];
pub const SHELL: &str = "/usr/bin/zsh";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error(
        "`{0}` is not a valid user name: lowercase letters, digits, `-` and `_`, \
         starting with a letter, at most 32 characters"
    )]
    Name(String),
    #[error("a user named `{0}` already exists")]
    Exists(String),
    #[error("the password is empty")]
    EmptyPassword,
    #[error("hashing the password: {0}")]
    Hash(String),
}

/// The account files with the user added.
#[derive(Debug, PartialEq, Eq)]
pub struct Files {
    pub passwd: String,
    pub group: String,
    pub shadow: String,
}

/// The name rules of `useradd`'s default, which every tool that reads
/// `/etc/passwd` accepts.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
}

/// Random bytes for a password's salt. Twelve, because SHA-crypt takes at
/// most 16 characters of salt, and 12 bytes are 16 in its base64; a longer
/// salt is cut to 16 by libxcrypt when it checks the password, which then
/// never matches the hash that was stored with all of it.
pub type Salt = [u8; 12];

/// Adds `name` with its own group, `FIRST_UID`, and `password` hashed with
/// SHA-512-crypt, the hash glibc's crypt and PAM verify by default.
pub fn add_first_user(
    files: &Files,
    name: &str,
    full_name: &str,
    password: &str,
    salt: &Salt,
) -> Result<Files, AccountError> {
    if !valid_name(name) {
        return Err(AccountError::Name(name.to_owned()));
    }
    if password.is_empty() {
        return Err(AccountError::EmptyPassword);
    }
    let exists = |table: &str| table.lines().any(|l| l.split(':').next() == Some(name));
    if exists(&files.passwd) || exists(&files.group) {
        return Err(AccountError::Exists(name.to_owned()));
    }
    let hash = ShaCrypt::default()
        .hash_password_with_salt(password.as_bytes(), salt)
        .map_err(|e| AccountError::Hash(e.to_string()))?;
    let gecos = full_name.replace([':', '\n'], " ");

    let mut passwd = files.passwd.clone();
    passwd.push_str(&format!(
        "{name}:x:{FIRST_UID}:{FIRST_UID}:{gecos}:/home/{name}:{SHELL}\n"
    ));

    let mut group = String::new();
    for line in files.group.lines() {
        let mut fields: Vec<String> = line.split(':').map(str::to_owned).collect();
        let is_admin = fields
            .first()
            .is_some_and(|g| ADMIN_GROUPS.contains(&g.as_str()));
        if is_admin {
            while fields.len() < 4 {
                fields.push(String::new());
            }
            if let Some(members) = fields.get_mut(3) {
                if !members.is_empty() {
                    members.push(',');
                }
                members.push_str(name);
            }
        }
        group.push_str(&fields.join(":"));
        group.push('\n');
    }
    group.push_str(&format!("{name}:x:{FIRST_UID}:\n"));

    let mut shadow = files.shadow.clone();
    shadow.push_str(&format!("{name}:{}:::::::\n", hash.as_str()));
    Ok(Files {
        passwd,
        group,
        shadow,
    })
}

/// The account at `FIRST_UID`, if there is one, and the files without it:
/// its passwd, shadow and own group lines, and its name in the admin
/// groups. First-boot setup takes back the account an unfinished setup
/// made, so the person can make it again after a restart.
pub fn without_first_user(files: &Files) -> Option<(String, Files)> {
    let first = FIRST_UID.to_string();
    let name = files.passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        (fields.nth(1)? == first).then(|| name.to_owned())
    })?;
    let keep = |table: &str| {
        table
            .lines()
            .filter(|l| l.split(':').next() != Some(name.as_str()))
            .map(|l| format!("{l}\n"))
            .collect::<String>()
    };
    let mut group = String::new();
    for line in keep(&files.group).lines() {
        let mut fields: Vec<String> = line.split(':').map(str::to_owned).collect();
        let is_admin = fields
            .first()
            .is_some_and(|g| ADMIN_GROUPS.contains(&g.as_str()));
        if is_admin && let Some(members) = fields.get_mut(3) {
            *members = members
                .split(',')
                .filter(|m| !m.is_empty() && *m != name)
                .collect::<Vec<_>>()
                .join(",");
        }
        group.push_str(&fields.join(":"));
        group.push('\n');
    }
    let files = Files {
        passwd: keep(&files.passwd),
        group,
        shadow: keep(&files.shadow),
    };
    Some((name, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha_crypt::PasswordVerifier;

    const SALT: Salt = *b"twelve bytes";

    fn base() -> Files {
        Files {
            passwd: "root:x:0:0:root:/root:/usr/bin/zsh\n".to_owned(),
            group: "root:x:0:\nwheel:x:10:\nusers:x:100:\n".to_owned(),
            shadow: String::new(),
        }
    }

    #[test]
    fn adds_an_administrator_with_a_verifiable_password() {
        let files = add_first_user(&base(), "youri", "Youri: Mattar", "s3cret", &SALT).unwrap();
        assert!(
            files
                .passwd
                .ends_with("youri:x:1000:1000:Youri  Mattar:/home/youri:/usr/bin/zsh\n")
        );
        assert!(files.group.contains("wheel:x:10:youri\n"));
        assert!(files.group.ends_with("youri:x:1000:\n"));
        let hash = files
            .shadow
            .strip_prefix("youri:")
            .and_then(|s| s.split(':').next())
            .unwrap();
        assert!(hash.starts_with("$6$"));
        // $6$rounds=N$SALT$HASH: no more salt than crypt(3) reads.
        let salt = hash.split('$').nth(3).unwrap();
        assert_eq!(salt.len(), 16, "{hash}");
        ShaCrypt::default()
            .verify_password(b"s3cret", hash)
            .unwrap();
    }

    #[test]
    fn the_first_user_is_taken_back_as_it_was_added() {
        let mut start = base();
        start.group = "root:x:0:\nwheel:x:10:admin\nusers:x:100:\n".to_owned();
        let added = add_first_user(&start, "youri", "Youri", "s3cret", &SALT).unwrap();
        let (name, back) = without_first_user(&added).unwrap();
        assert_eq!(name, "youri");
        assert_eq!(back, start);
        assert!(without_first_user(&base()).is_none());
    }

    #[test]
    fn refuses_bad_names_duplicates_and_empty_passwords() {
        for bad in ["", "Youri", "1abc", "a b", "a:b", &"a".repeat(33)] {
            assert_eq!(
                add_first_user(&base(), bad, "", "x", &SALT),
                Err(AccountError::Name(bad.to_owned()))
            );
        }
        assert_eq!(
            add_first_user(&base(), "root", "", "x", &SALT),
            Err(AccountError::Exists("root".to_owned()))
        );
        assert_eq!(
            add_first_user(&base(), "youri", "", "", &SALT),
            Err(AccountError::EmptyPassword)
        );
    }
}
