//! System users and groups, declared by the packages that need them in
//! `sysusers.d` files and created in `/etc/passwd` and `/etc/group` at boot.
//!
//! The format is systemd's, so upstream packages' files work as shipped. The
//! subset: `u NAME ID "GECOS" HOME SHELL`, `g NAME ID`, `m USER GROUP`. `r`
//! (ranges) is accepted and ignored: system IDs are always allocated down
//! from 999. A user or group that already exists is left as it is, so this
//! is safe to run at every boot, and an operator's edits survive.
//!
//! At every boot rather than once at install, because an update can bring a
//! package with a new user, and `/etc` is the machine's, not the image's.

use std::collections::BTreeSet;

/// The highest ID allocated to a system user or group; regular users start
/// at 1000.
pub const SYSTEM_MAX: u32 = 999;
/// The lowest. Below it are IDs the base system assigns by hand.
pub const SYSTEM_MIN: u32 = 100;
/// The shell of an account nobody logs in to.
pub const NO_SHELL: &str = "/usr/bin/false";

#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    User {
        name: String,
        /// `None`: allocate.
        uid: Option<u32>,
        gid: Option<u32>,
        gecos: String,
        home: String,
        shell: String,
    },
    Group {
        name: String,
        gid: Option<u32>,
    },
    Member {
        user: String,
        group: String,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

/// Parses one `sysusers.d` file.
pub fn parse(text: &str) -> Result<Vec<Entry>, ParseError> {
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let Some(fields) = crate::config::fields(line) else {
            continue;
        };
        let error = |message: &str| ParseError {
            line: index + 1,
            message: message.to_owned(),
        };
        let field = |i: usize| fields.get(i).map(String::as_str).filter(|f| *f != "-");
        let id = |f: Option<&str>| -> Result<Option<u32>, ParseError> {
            match f {
                None => Ok(None),
                Some(text) => text
                    .parse()
                    .map(Some)
                    .map_err(|_| error(&format!("`{text}` is not a numeric ID"))),
            }
        };
        if field(0) == Some("r") {
            continue;
        }
        let name = field(1).ok_or_else(|| error("no name"))?.to_owned();
        match field(0) {
            Some("u") => {
                // "uid:gid" names the primary group by ID.
                let (uid, gid) = match field(2) {
                    Some(text) => match text.split_once(':') {
                        Some((u, g)) => (id(Some(u))?, id(Some(g))?),
                        None => (id(Some(text))?, None),
                    },
                    None => (None, None),
                };
                entries.push(Entry::User {
                    name,
                    uid,
                    gid,
                    gecos: field(3).unwrap_or("").to_owned(),
                    home: field(4).unwrap_or("/").to_owned(),
                    shell: field(5).unwrap_or(NO_SHELL).to_owned(),
                });
            }
            Some("g") => entries.push(Entry::Group {
                name,
                gid: id(field(2))?,
            }),
            Some("m") => entries.push(Entry::Member {
                user: name,
                group: field(2)
                    .ok_or_else(|| error("`m` needs a group"))?
                    .to_owned(),
            }),
            Some(other) => return Err(error(&format!("unknown type `{other}`"))),
            None => return Err(error("no type")),
        }
    }
    Ok(entries)
}

/// One line of `/etc/passwd` or `/etc/group`, split on `:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record(pub Vec<String>);

impl Record {
    fn parse(line: &str) -> Option<Record> {
        let line = line.trim_end();
        (!line.is_empty() && !line.starts_with('#'))
            .then(|| Record(line.split(':').map(str::to_owned).collect()))
    }
    fn name(&self) -> &str {
        self.0.first().map(String::as_str).unwrap_or("")
    }
    fn id(&self) -> Option<u32> {
        self.0.get(2).and_then(|f| f.parse().ok())
    }
}

/// The account database as text, and what applying entries changes.
pub struct Database {
    pub passwd: Vec<Record>,
    pub group: Vec<Record>,
    /// Names given a locked password, for `/etc/shadow`.
    pub new_users: Vec<String>,
}

impl Database {
    pub fn parse(passwd: &str, group: &str) -> Database {
        Database {
            passwd: passwd.lines().filter_map(Record::parse).collect(),
            group: group.lines().filter_map(Record::parse).collect(),
            new_users: Vec::new(),
        }
    }

    pub fn passwd_text(&self) -> String {
        render(&self.passwd)
    }

    pub fn group_text(&self) -> String {
        render(&self.group)
    }

    fn group_gid(&self, name: &str) -> Option<u32> {
        self.group
            .iter()
            .find(|r| r.name() == name)
            .and_then(Record::id)
    }

    /// The highest free ID in the system range, for a user and a group of
    /// the same name when both are free, as systemd does.
    fn allocate(&self, pair: bool) -> Option<u32> {
        let users: BTreeSet<u32> = self.passwd.iter().filter_map(Record::id).collect();
        let groups: BTreeSet<u32> = self.group.iter().filter_map(Record::id).collect();
        (SYSTEM_MIN..=SYSTEM_MAX)
            .rev()
            .find(|id| !groups.contains(id) && (!pair || !users.contains(id)))
    }

    fn add_group(&mut self, name: &str, gid: Option<u32>) -> Result<u32, String> {
        if let Some(existing) = self.group_gid(name) {
            return Ok(existing);
        }
        let gid = match gid {
            Some(gid) => gid,
            None => self.allocate(false).ok_or("no free system group ID left")?,
        };
        self.group.push(Record(vec![
            name.to_owned(),
            "x".to_owned(),
            gid.to_string(),
            String::new(),
        ]));
        Ok(gid)
    }

    /// Applies entries: groups first, then users, then memberships, as
    /// systemd-sysusers does — a file may add its user to a group another
    /// file declares, and files are read in name order, not dependency
    /// order. Returns a description of each change.
    pub fn apply(&mut self, entries: &[Entry]) -> Result<Vec<String>, String> {
        let mut changes = Vec::new();
        let phase = |e: &&Entry| match e {
            Entry::Group { .. } => 0,
            Entry::User { .. } => 1,
            Entry::Member { .. } => 2,
        };
        let mut ordered: Vec<&Entry> = entries.iter().collect();
        ordered.sort_by_key(phase);
        for entry in ordered {
            match entry {
                Entry::Group { name, gid } => {
                    if self.group_gid(name).is_none() {
                        let gid = self.add_group(name, *gid)?;
                        changes.push(format!("group {name} ({gid})"));
                    }
                }
                Entry::User {
                    name,
                    uid,
                    gid,
                    gecos,
                    home,
                    shell,
                } => {
                    if self.passwd.iter().any(|r| r.name() == name) {
                        continue;
                    }
                    let uid = match uid {
                        Some(uid) => *uid,
                        None => match self.group_gid(name) {
                            // A group of the same name exists: share its
                            // ID if no user has it.
                            Some(g) if !self.passwd.iter().any(|r| r.id() == Some(g)) => g,
                            _ => self.allocate(true).ok_or("no free system user ID left")?,
                        },
                    };
                    let gid = match gid {
                        Some(gid) => *gid,
                        None => self.add_group(
                            name,
                            Some(uid).filter(|u| !self.group.iter().any(|r| r.id() == Some(*u))),
                        )?,
                    };
                    self.passwd.push(Record(vec![
                        name.clone(),
                        "x".to_owned(),
                        uid.to_string(),
                        gid.to_string(),
                        gecos.clone(),
                        home.clone(),
                        shell.clone(),
                    ]));
                    self.new_users.push(name.clone());
                    changes.push(format!("user {name} ({uid}:{gid})"));
                }
                Entry::Member { user, group } => {
                    let record = self
                        .group
                        .iter_mut()
                        .find(|r| r.name() == group)
                        .ok_or_else(|| format!("`m {user} {group}`: no group {group}"))?;
                    while record.0.len() < 4 {
                        record.0.push(String::new());
                    }
                    if let Some(members) = record.0.get_mut(3)
                        && !members.split(',').any(|m| m == user)
                    {
                        if !members.is_empty() {
                            members.push(',');
                        }
                        members.push_str(user);
                        changes.push(format!("{user} in {group}"));
                    }
                }
            }
        }
        Ok(changes)
    }
}

fn render(records: &[Record]) -> String {
    records
        .iter()
        .map(|r| format!("{}\n", r.0.join(":")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/usr/bin/zsh\n\
                          nobody:x:65534:65534:Unprivileged user:/:/usr/bin/false\n";
    const GROUP: &str = "root:x:0:\nwheel:x:10:\nnogroup:x:65534:\n";

    #[test]
    fn parses_the_subset() {
        let entries = parse(
            "# comment\n\
             u messagebus - \"D-Bus daemon\" /run/dbus\n\
             u greeter 960:960 - /var/lib/greeter /usr/bin/zsh\n\
             g video 44\n\
             m greeter video\n\
             r - 500-900\n",
        )
        .unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries.first().unwrap(),
            &Entry::User {
                name: "messagebus".to_owned(),
                uid: None,
                gid: None,
                gecos: "D-Bus daemon".to_owned(),
                home: "/run/dbus".to_owned(),
                shell: NO_SHELL.to_owned(),
            }
        );
    }

    #[test]
    fn rejects_what_it_does_not_understand() {
        assert!(parse("u bad notanumber").is_err());
        assert!(parse("z thing").is_err());
        assert!(parse("m lonely").is_err());
    }

    #[test]
    fn allocates_down_from_999_with_a_matching_group() {
        let mut db = Database::parse(PASSWD, GROUP);
        let entries = parse("u messagebus -\nu polkitd -\ng render -\n").unwrap();
        db.apply(&entries).unwrap();
        // Groups are allocated first, then each user with a group of its
        // own ID.
        assert!(db.group_text().contains("render:x:999:\n"));
        assert!(
            db.passwd_text()
                .contains("messagebus:x:998:998::/:/usr/bin/false\n")
        );
        assert!(db.passwd_text().contains("polkitd:x:997:997:"));
        assert!(db.group_text().contains("messagebus:x:998:\n"));
        assert_eq!(db.new_users, ["messagebus", "polkitd"]);
    }

    #[test]
    fn running_twice_changes_nothing_the_second_time() {
        let entries = parse("u messagebus -\ng video 44\nm messagebus video\n").unwrap();
        let mut db = Database::parse(PASSWD, GROUP);
        assert_eq!(db.apply(&entries).unwrap().len(), 3);
        let mut again = Database::parse(&db.passwd_text(), &db.group_text());
        assert!(again.apply(&entries).unwrap().is_empty());
        assert_eq!(again.passwd_text(), db.passwd_text());
        assert_eq!(again.group_text(), db.group_text());
        assert!(db.group_text().contains("video:x:44:messagebus\n"));
    }

    #[test]
    fn existing_accounts_are_left_alone() {
        let mut db = Database::parse(PASSWD, GROUP);
        db.apply(&parse("u root 5 \"changed\" /elsewhere\ng wheel 99\n").unwrap())
            .unwrap();
        assert_eq!(db.passwd_text(), PASSWD);
        assert_eq!(db.group_text(), GROUP);
    }

    #[test]
    fn memberships_wait_for_groups_declared_later() {
        // cosmic-greeter.conf sorts before devices.conf, which declares video.
        let mut entries = parse("u cosmic-greeter -\nm cosmic-greeter video\n").unwrap();
        entries.extend(parse("g video -\n").unwrap());
        let mut db = Database::parse(PASSWD, GROUP);
        db.apply(&entries).unwrap();
        assert!(db.group_text().contains(":cosmic-greeter\n"));
        assert!(
            db.group_text()
                .lines()
                .any(|l| l.starts_with("video:") && l.ends_with(":cosmic-greeter"))
        );
    }

    #[test]
    fn membership_needs_the_group() {
        let mut db = Database::parse(PASSWD, GROUP);
        assert!(db.apply(&parse("m root missing").unwrap()).is_err());
    }
}
