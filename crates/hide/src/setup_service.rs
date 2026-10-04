//! `os.hide.Setup1`, on hideupd: what first-boot setup changes, done as
//! root for `hidesetup`, which runs as the greeter's user. See
//! ARCHITECTURE.md, "First-boot setup".
//!
//! Anyone may read what it offers. Changing anything is the greeter's
//! user's, or root's, and only until setup is done: after that every
//! change is refused, and the Settings pages are how a machine changes.

use std::fs;
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use zbus::message::Header;
use zbus::{Connection, fdo, interface};

use hide::firstboot;

pub const PATH: &str = "/os/hide/Setup1";
/// Present once setup is done; hideos-greeter starts the greeter then.
pub const SETUP_DONE: &str = "/var/lib/hide/setup-done";
/// On the ESP: the random key an encrypted disk is made with, until setup
/// gives it the person's passphrase. hidestage tries it first.
pub const SETUP_KEY: &str = "EFI/hideos/setup.key";
const GREETER_USER: &str = "cosmic-greeter";
const GREETER_HOME: &str = "/var/lib/cosmic-greeter";
const XKB_BASE: &str = "/usr/share/xkeyboard-config-2/rules/base.lst";
const ZONE_TAB: &str = "/usr/share/zoneinfo/zone1970.tab";
const KEYBOARD_FILE: &str = ".config/cosmic/com.system76.CosmicComp/v1/xkb_config";
/// The clock's 24-hour setting, which the panel's and the greeter's clocks
/// read.
const CLOCK_FILE: &str = ".config/cosmic/com.system76.CosmicAppletTime/v1/military_time";

#[derive(Default)]
pub struct Setup1 {
    /// The keyboard chosen, for the account made after it.
    keyboard: Mutex<Option<(String, String)>>,
    /// Whether the language chosen reads the time on a 24-hour clock.
    clock24: Mutex<Option<bool>>,
    /// The account made, for the disk's passphrase and nothing else.
    account: Mutex<Option<String>>,
}

#[interface(name = "os.hide.Setup1")]
impl Setup1 {
    /// Whether setup still has to run.
    async fn needed(&self) -> bool {
        !Path::new(SETUP_DONE).exists()
    }

    /// The languages the system has, as `LANG` takes them.
    async fn languages(&self) -> fdo::Result<Vec<String>> {
        let out = output(Command::new("locale").arg("-a")).await?;
        Ok(firstboot::languages(&out))
    }

    /// Keyboard layouts: name and description.
    async fn layouts(&self) -> fdo::Result<Vec<(String, String)>> {
        let text = fs::read_to_string(XKB_BASE).map_err(failed)?;
        Ok(firstboot::layouts(&text)
            .into_iter()
            .map(|l| (l.name, l.description))
            .collect())
    }

    /// Time zones, as `Area/City`.
    async fn zones(&self) -> fdo::Result<Vec<String>> {
        let text = fs::read_to_string(ZONE_TAB).map_err(failed)?;
        Ok(firstboot::zones(&text))
    }

    /// Whether the machine already reaches the network — a cable — so the
    /// Wi-Fi page can be passed.
    async fn online(&self) -> bool {
        output(Command::new("nmcli").args(["-t", "-f", "STATE", "general"]))
            .await
            .is_ok_and(|state| state.trim().starts_with("connected"))
    }

    /// Wi-Fi networks in range: name, signal 0–100, whether it asks for a
    /// password. Strongest first, each name once.
    async fn wifi_networks(&self) -> fdo::Result<Vec<(String, u32, bool)>> {
        let out = output(Command::new("nmcli").args([
            "-t",
            "-f",
            "SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "--rescan",
            "auto",
        ]))
        .await?;
        Ok(wifi_list(&out))
    }

    /// Whether the disk waits for the person's passphrase: encrypted with
    /// the installer's setup key, which is still on the ESP. A reinstall
    /// that kept the disk's own passphrase does not, nor a disk that is
    /// not encrypted.
    async fn disk_needs_passphrase(&self) -> bool {
        crate::deploy::Esp::mount()
            .map(|esp| esp.path().join(SETUP_KEY).exists())
            .unwrap_or(false)
    }

    async fn set_language(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        language: String,
    ) -> fdo::Result<()> {
        allowed(connection, &header).await?;
        if !firstboot::valid_language(&language) {
            return Err(fdo::Error::InvalidArgs(format!(
                "`{language}` is not a language"
            )));
        }
        let existing = fs::read_to_string("/etc/environment").unwrap_or_default();
        write(
            "/etc/environment",
            &firstboot::environment_with(&existing, &language),
            0o644,
        )
        .map_err(failed)?;
        // The clock as the language's region reads it, as on a Mac: on
        // the login screen now, and in the account made after.
        let clock24 = firstboot::uses_24_hour_clock(&language);
        clock_for(Path::new(GREETER_HOME), GREETER_USER, clock24).map_err(failed)?;
        if let Ok(mut chosen) = self.clock24.lock() {
            *chosen = Some(clock24);
        }
        say(&format!("language {language}"));
        Ok(())
    }

    /// The keyboard, now for setup itself — the greeter's compositor reads
    /// its user's setting as it changes — and later for the account.
    async fn set_keyboard(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        layout: String,
        variant: String,
    ) -> fdo::Result<()> {
        allowed(connection, &header).await?;
        if !firstboot::valid_layout(&layout)
            || !(variant.is_empty() || firstboot::valid_layout(&variant))
        {
            return Err(fdo::Error::InvalidArgs(format!(
                "`{layout}` is not a layout"
            )));
        }
        keyboard_for(Path::new(GREETER_HOME), GREETER_USER, &layout, &variant).map_err(failed)?;
        if let Ok(mut chosen) = self.keyboard.lock() {
            *chosen = Some((layout.clone(), variant));
        }
        say(&format!("keyboard {layout}"));
        Ok(())
    }

    async fn set_zone(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        zone: String,
    ) -> fdo::Result<()> {
        allowed(connection, &header).await?;
        let target = Path::new("/usr/share/zoneinfo").join(&zone);
        if !firstboot::valid_zone(&zone) || !target.is_file() {
            return Err(fdo::Error::InvalidArgs(format!(
                "`{zone}` is not a time zone"
            )));
        }
        let _ = fs::remove_file("/etc/localtime");
        symlink(&target, "/etc/localtime").map_err(failed)?;
        say(&format!("time zone {zone}"));
        Ok(())
    }

    /// Joins a Wi-Fi network; NetworkManager keeps it for every boot after.
    async fn connect_wifi(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        ssid: String,
        password: String,
    ) -> fdo::Result<()> {
        allowed(connection, &header).await?;
        let mut command = Command::new("nmcli");
        command.args(["device", "wifi", "connect"]).arg(&ssid);
        if !password.is_empty() {
            // On nmcli's command line, readable in /proc while it runs:
            // before setup there is no other account to read it.
            command.arg("password").arg(&password);
        }
        output(&mut command).await?;
        say(&format!("joined {ssid}"));
        Ok(())
    }

    /// The person's account: an administrator, as the first account on a
    /// Mac is, with the keyboard chosen before.
    async fn create_account(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        full_name: String,
        login: String,
        password: String,
    ) -> fdo::Result<()> {
        allowed(connection, &header).await?;
        if !firstboot::valid_full_name(&full_name) {
            return Err(fdo::Error::InvalidArgs(
                "the name cannot hold `:` or `,`".into(),
            ));
        }
        let keyboard = self.keyboard.lock().ok().and_then(|k| k.clone());
        let clock24 = self.clock24.lock().ok().and_then(|c| *c);
        create_account(&full_name, &login, &password, keyboard, clock24).map_err(failed)?;
        if let Ok(mut account) = self.account.lock() {
            *account = Some(login.clone());
        }
        say(&format!("account {login} created"));
        Ok(())
    }

    /// The person's password becomes the disk's passphrase, a recovery key
    /// is added, and the setup key goes. Returns the recovery key, which
    /// is shown once and kept nowhere.
    async fn secure_disk(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        password: String,
    ) -> fdo::Result<String> {
        allowed(connection, &header).await?;
        let made = self.account.lock().ok().and_then(|a| a.clone());
        if made.is_none() {
            return Err(fdo::Error::Failed(
                "the account comes before the disk".into(),
            ));
        }
        let key = secure_disk(&password).await.map_err(failed)?;
        say("the disk has the person's passphrase and a recovery key");
        Ok(key)
    }

    /// Setup is done: the mark is written and this interface refuses
    /// everything from now on.
    async fn finish(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<()> {
        allowed(connection, &header).await?;
        let made = self.account.lock().ok().and_then(|a| a.clone());
        if made.is_none() && !has_person() {
            return Err(fdo::Error::Failed("setup needs an account first".into()));
        }
        fs::create_dir_all("/var/lib/hide").map_err(failed)?;
        write(SETUP_DONE, "", 0o644).map_err(failed)?;
        say("setup is done");
        Ok(())
    }
}

/// The greeter's user or root, and only while setup is needed.
async fn allowed(connection: &Connection, header: &Header<'_>) -> fdo::Result<()> {
    if Path::new(SETUP_DONE).exists() {
        return Err(fdo::Error::AccessDenied("setup is done".into()));
    }
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("a call with no sender".into()))?
        .to_owned();
    let bus = fdo::DBusProxy::new(connection).await?;
    let uid = bus.get_connection_unix_user(sender.into()).await?;
    let greeter = fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| uid_of(&passwd, GREETER_USER));
    if uid == 0 || Some(uid) == greeter {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(
            "first-boot setup is the greeter's".into(),
        ))
    }
}

fn uid_of(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(name))
            .then(|| fields.nth(1)?.parse().ok())
            .flatten()
    })
}

/// Whether a person's account exists: one in the range accounts are made
/// in. A machine set up by hand gets through `finish` too.
fn has_person() -> bool {
    fs::read_to_string("/etc/passwd").is_ok_and(|passwd| {
        passwd.lines().any(|line| {
            line.split(':')
                .nth(2)
                .and_then(|uid| uid.parse::<u32>().ok())
                .is_some_and(|uid| (1000..60_000).contains(&uid))
        })
    })
}

fn create_account(
    full_name: &str,
    login: &str,
    password: &str,
    keyboard: Option<(String, String)>,
    clock24: Option<bool>,
) -> Result<()> {
    let etc = Path::new("/etc");
    let read = |file: &str| fs::read_to_string(etc.join(file)).unwrap_or_default();
    let mut files = hide::account::Files {
        passwd: read("passwd"),
        group: read("group"),
        shadow: read("shadow"),
    };
    // An account here was made by a setup the machine restarted out of
    // before `finish`: no one has logged in to it, since the login screen
    // waits for setup. It is taken back, and its home with it if the new
    // account has another name, so it can be made again.
    if let Some((earlier, without)) = hide::account::without_first_user(&files) {
        if earlier != login {
            let _ = fs::remove_dir_all(Path::new("/home").join(&earlier));
        }
        say(&format!(
            "account {earlier} from an unfinished setup taken back"
        ));
        files = without;
    }
    let mut salt: hide::account::Salt = [0; 12];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut salt))
        .context("reading /dev/urandom")?;
    let files = hide::account::add_first_user(&files, login, full_name, password, &salt)?;
    write("/etc/passwd", &files.passwd, 0o644)?;
    write("/etc/group", &files.group, 0o644)?;
    write("/etc/shadow", &files.shadow, 0o600)?;

    let home = Path::new("/home").join(login);
    fs::create_dir_all(&home)?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
    chown(&home, hide::account::FIRST_UID)?;
    if let Some((layout, variant)) = keyboard {
        keyboard_for(&home, login, &layout, &variant)?;
    }
    if let Some(clock24) = clock24 {
        clock_for(&home, login, clock24)?;
    }
    // Rootless containers: the person's subordinate IDs, as `hide setup`
    // gives them at boot.
    crate::setup::subordinate_ids(Path::new("/"))?;
    Ok(())
}

/// `user`'s COSMIC keyboard setting, under `home`, owned by them.
fn keyboard_for(home: &Path, user: &str, layout: &str, variant: &str) -> Result<()> {
    cosmic_setting(
        home,
        user,
        KEYBOARD_FILE,
        &firstboot::xkb_config(layout, variant),
    )
}

/// `user`'s 24-hour clock setting, under `home`.
fn clock_for(home: &Path, user: &str, clock24: bool) -> Result<()> {
    cosmic_setting(
        home,
        user,
        CLOCK_FILE,
        if clock24 { "true" } else { "false" },
    )
}

/// A COSMIC setting of `user`'s, `relative` under `home`, owned by them.
fn cosmic_setting(home: &Path, user: &str, relative: &str, text: &str) -> Result<()> {
    let file = home.join(relative);
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let uid = uid_of(&passwd, user).with_context(|| format!("no account {user}"))?;
    let mut dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&dir)?;
    write(&file, text, 0o644)?;
    // Every directory made under the home is the user's, not root's.
    chown(&file, uid)?;
    while dir.starts_with(home) && dir != home {
        chown(&dir, uid)?;
        dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
    }
    Ok(())
}

async fn secure_disk(password: &str) -> Result<String> {
    ensure!(!password.is_empty(), "the passphrase is empty");
    let device = luks_device().await?;
    let esp = crate::deploy::Esp::mount()?;
    let key_path = esp.path().join(SETUP_KEY);
    let setup_key =
        fs::read(&key_path).context("the disk has no setup key: is it set up already?")?;
    let mut random = [0u8; 25];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .context("reading /dev/urandom")?;
    let recovery = hide::recovery::format_key(&random);

    let work = Path::new("/run/hide-setup");
    fs::create_dir_all(work)?;
    fs::set_permissions(work, fs::Permissions::from_mode(0o700))?;
    let old = work.join("key.setup");
    write(&old, "", 0o600)?;
    fs::write(&old, &setup_key)?;
    let result = async {
        for (what, new) in [
            ("passphrase", password),
            ("recovery key", recovery.as_str()),
        ] {
            let fresh = work.join("key.new");
            write(&fresh, new, 0o600)?;
            let added = output(
                Command::new("cryptsetup")
                    .args(["luksAddKey", "--batch-mode", "--key-file"])
                    .arg(&old)
                    .arg(&device)
                    .arg(&fresh),
            )
            .await;
            let _ = fs::remove_file(&fresh);
            added.map_err(|e| anyhow::anyhow!("adding the {what}: {e}"))?;
        }
        // The setup key's own slot, last, once the two that replace it
        // are there.
        output(
            Command::new("cryptsetup")
                .args(["luksRemoveKey", "--batch-mode"])
                .arg(&device)
                .arg(&old),
        )
        .await
        .map_err(|e| anyhow::anyhow!("removing the setup key: {e}"))?;
        anyhow::Ok(())
    }
    .await;
    let _ = fs::remove_file(&old);
    result?;
    fs::remove_file(&key_path).context("removing the setup key from the ESP")?;
    rustix::fs::sync();
    esp.unmount()?;
    Ok(recovery)
}

/// The LUKS2 partition under the mapped root.
async fn luks_device() -> Result<PathBuf> {
    let status = output(Command::new("cryptsetup").args(["status", "hideos-root"]))
        .await
        .map_err(|e| anyhow::anyhow!("the root is not encrypted: {e}"))?;
    status
        .lines()
        .find_map(|line| line.trim().strip_prefix("device:"))
        .map(|device| PathBuf::from(device.trim()))
        .context("cryptsetup does not say which device the root is on")
}

/// `nmcli -t` lines — SSID:SIGNAL:SECURITY, with `\:` for a colon in a
/// name — as networks, strongest first, each name once.
fn wifi_list(out: &str) -> Vec<(String, u32, bool)> {
    let mut networks: Vec<(String, u32, bool)> = Vec::new();
    for line in out.lines() {
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut escaped = false;
        for c in line.chars() {
            match (escaped, c) {
                (true, c) => {
                    field.push(c);
                    escaped = false;
                }
                (false, '\\') => escaped = true,
                (false, ':') => fields.push(std::mem::take(&mut field)),
                (false, c) => field.push(c),
            }
        }
        fields.push(field);
        let [ssid, signal, security] = fields.as_slice() else {
            continue;
        };
        if ssid.is_empty() {
            continue;
        }
        let signal = signal.parse().unwrap_or(0);
        let secured = !security.is_empty() && security != "--";
        match networks.iter_mut().find(|(name, ..)| name == ssid) {
            Some(known) if known.1 < signal => *known = (ssid.clone(), signal, secured),
            Some(_) => {}
            None => networks.push((ssid.clone(), signal, secured)),
        }
    }
    networks.sort_by_key(|network| std::cmp::Reverse(network.1));
    networks
}

async fn output(command: &mut Command) -> fdo::Result<String> {
    let out = command
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| fdo::Error::Failed(format!("{command:?}: {e}")))?;
    if !out.status.success() {
        return Err(fdo::Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write(path: impl AsRef<Path>, text: &str, mode: u32) -> Result<()> {
    let path = path.as_ref();
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn chown(path: &Path, id: u32) -> Result<()> {
    rustix::fs::chown(
        path,
        Some(rustix::fs::Uid::from_raw(id)),
        Some(rustix::fs::Gid::from_raw(id)),
    )
    .with_context(|| format!("giving {} to {id}", path.display()))
}

fn failed(error: impl std::fmt::Display) -> fdo::Error {
    fdo::Error::Failed(format!("{error:#}"))
}

fn say(line: &str) {
    eprintln!("hideupd: setup: {line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_names_are_unescaped_and_kept_once() {
        let out = "Home:70:WPA2\nCafé\\: free:40:\nHome:80:WPA2\n:30:WPA2\n";
        assert_eq!(
            wifi_list(out),
            [
                ("Home".to_owned(), 80, true),
                ("Café: free".to_owned(), 40, false)
            ]
        );
    }

    #[test]
    fn uids_come_from_passwd() {
        let passwd = "root:x:0:0::/root:/bin/sh\ncosmic-greeter:x:991:991::/var/lib/cosmic-greeter:/usr/bin/nologin\n";
        assert_eq!(uid_of(passwd, "cosmic-greeter"), Some(991));
        assert_eq!(uid_of(passwd, "nobody"), None);
    }
}
