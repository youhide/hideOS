//! What first-boot setup decides, apart from doing it: the languages,
//! keyboard layouts and time zones it offers, read from the files that
//! define them, and what it writes for each choice. hideupd's
//! `os.hide.Setup1` does the writing; see ARCHITECTURE.md, "First-boot
//! setup".

/// A keyboard layout as xkeyboard-config names and describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub name: String,
    pub description: String,
}

/// The layouts in xkeyboard-config's `base.lst`, the `! layout` section:
/// one per line, the name, spaces, the description. Sorted by description,
/// as a person looks for one.
pub fn layouts(base_lst: &str) -> Vec<Layout> {
    let mut layouts: Vec<Layout> = base_lst
        .lines()
        .skip_while(|line| line.trim() != "! layout")
        .skip(1)
        .take_while(|line| !line.starts_with('!'))
        .filter_map(|line| {
            let line = line.trim();
            let (name, description) = line.split_once(char::is_whitespace)?;
            Some(Layout {
                name: name.to_owned(),
                description: description.trim().to_owned(),
            })
        })
        .filter(|layout| valid_layout(&layout.name))
        .collect();
    layouts.sort_by(|a, b| a.description.cmp(&b.description));
    layouts
}

/// What a layout name may be: xkeyboard-config's are lowercase letters and
/// digits, and the name ends up in a file and on a command line.
pub fn valid_layout(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The time zones in tzdata's `zone1970.tab`: the third column of every
/// line that is not a comment. Sorted.
pub fn zones(zone1970_tab: &str) -> Vec<String> {
    let mut zones: Vec<String> = zone1970_tab
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split('\t').nth(2))
        .map(str::to_owned)
        .filter(|zone| valid_zone(zone))
        .collect();
    zones.sort();
    zones.dedup();
    zones
}

/// A zone is a relative path under /usr/share/zoneinfo: `Area/City` or
/// `Area/Sub/City`, never `..`, never absolute.
pub fn valid_zone(zone: &str) -> bool {
    !zone.is_empty()
        && !zone.starts_with('/')
        && zone.split('/').all(|part| {
            !part.is_empty()
                && part != ".."
                && part != "."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-+".contains(&b))
        })
}

/// The languages a person can choose: the UTF-8 locales `locale -a`
/// lists, under the name a `LANG` takes — `pt_BR.UTF-8` — without C and
/// POSIX, which are no one's language. Sorted.
pub fn languages(locale_a: &str) -> Vec<String> {
    let mut languages: Vec<String> = locale_a
        .lines()
        .map(str::trim)
        .filter_map(|locale| {
            let (name, codeset) = locale.split_once('.')?;
            let utf8 =
                codeset.eq_ignore_ascii_case("utf8") || codeset.eq_ignore_ascii_case("utf-8");
            (utf8 && name != "C" && name != "POSIX" && valid_language(&format!("{name}.UTF-8")))
                .then(|| format!("{name}.UTF-8"))
        })
        .collect();
    languages.sort();
    languages.dedup();
    languages
}

/// `ll_CC.UTF-8`, as `LANG` takes it.
pub fn valid_language(language: &str) -> bool {
    let Some((name, "UTF-8")) = language.split_once('.') else {
        return false;
    };
    let Some((lang, country)) = name.split_once('_') else {
        return false;
    };
    (2..=3).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_lowercase())
        && country.len() == 2
        && country.bytes().all(|b| b.is_ascii_uppercase())
}

/// `/etc/environment` with `LANG` set to `language`: the line replaced if
/// there is one, added if not, everything else as it was. pam_env reads
/// it after its vendor file, so it is the machine's choice over hideOS's.
pub fn environment_with(existing: &str, language: &str) -> String {
    let mut out = String::new();
    let mut set = false;
    for line in existing.lines() {
        if line.trim_start().starts_with("LANG=") {
            if !set {
                out.push_str(&format!("LANG={language}\n"));
                set = true;
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !set {
        out.push_str(&format!("LANG={language}\n"));
    }
    out
}

/// cosmic-comp's keyboard setting, `com.system76.CosmicComp/v1/xkb_config`,
/// as the RON it reads. The repeat values are its defaults.
pub fn xkb_config(layout: &str, variant: &str) -> String {
    format!(
        "(\n    rules: \"\",\n    model: \"pc104\",\n    layout: \"{layout}\",\n    \
         variant: \"{variant}\",\n    options: None,\n    repeat_delay: 600,\n    \
         repeat_rate: 25,\n)\n"
    )
}

/// Whether a language's region reads the time on a 24-hour clock: all
/// but the few that use AM and PM.
pub fn uses_24_hour_clock(language: &str) -> bool {
    let name = language.split('.').next().unwrap_or_default();
    !matches!(name, "en_US" | "en_CA" | "en_AU" | "en_PH" | "es_MX")
}

/// A full name for the account, as the GECOS field can hold it: one line,
/// no `:`, which separates the fields.
pub fn valid_full_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.contains([':', '\n', ','])
}

/// A login suggested from a full name, as a Mac suggests one: the first
/// name, lowercased, accents folded, with what a login cannot hold left
/// out. Empty when
/// nothing is left.
pub fn suggested_login(full_name: &str) -> String {
    let first = full_name.split_whitespace().next().unwrap_or_default();
    let mut login: String = first
        .chars()
        .flat_map(char::to_lowercase)
        .map(fold)
        .collect();
    while login.starts_with(|c: char| !c.is_ascii_lowercase()) {
        login.remove(0);
    }
    login.truncate(32);
    login
}

/// A letter as a login can hold it: accents folded as a Mac folds them,
/// José to jose; what has no ASCII letter, nothing.
fn fold(c: char) -> &'static str {
    match c {
        'a' | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'c' | 'ç' | 'ć' | 'č' => "c",
        'd' | 'ď' | 'đ' => "d",
        'e' | 'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'g' | 'ğ' => "g",
        'i' | 'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => "i",
        'l' | 'ł' | 'ľ' => "l",
        'n' | 'ñ' | 'ń' | 'ň' => "n",
        'o' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'r' | 'ř' => "r",
        's' | 'ś' | 'š' | 'ș' | 'ş' => "s",
        't' | 'ť' | 'ț' | 'ţ' => "t",
        'u' | 'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'y' | 'ý' | 'ÿ' => "y",
        'z' | 'ź' | 'ż' | 'ž' => "z",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'b' => "b",
        'f' => "f",
        'h' => "h",
        'j' => "j",
        'k' => "k",
        'm' => "m",
        'p' => "p",
        'q' => "q",
        'v' => "v",
        'w' => "w",
        'x' => "x",
        '0' => "0",
        '1' => "1",
        '2' => "2",
        '3' => "3",
        '4' => "4",
        '5' => "5",
        '6' => "6",
        '7' => "7",
        '8' => "8",
        '9' => "9",
        '-' => "-",
        '_' => "_",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_come_from_the_layout_section() {
        let lst = "! model\n  pc104  Generic 104-key PC\n\n! layout\n  us   English (US)\n  br   Portuguese (Brazil)\n  de   German\n\n! variant\n  intl  us: English (US, intl.)\n";
        let found = layouts(lst);
        let names: Vec<&str> = found.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["us", "de", "br"]);
        assert_eq!(
            found.get(2).map(|l| l.description.as_str()),
            Some("Portuguese (Brazil)")
        );
    }

    #[test]
    fn zones_are_the_third_column() {
        let tab = "# comment\nBR\t-2332-04637\tAmerica/Sao_Paulo\tSE (GO, DF)\nPT\t+3843-00908\tEurope/Lisbon\tPortugal\n";
        assert_eq!(zones(tab), ["America/Sao_Paulo", "Europe/Lisbon"]);
    }

    #[test]
    fn zones_stay_under_zoneinfo() {
        assert!(valid_zone("America/Argentina/Buenos_Aires"));
        assert!(valid_zone("Etc/GMT+3"));
        for bad in [
            "",
            "/etc/passwd",
            "../../etc/shadow",
            "America/../..",
            "a//b",
            "a b",
        ] {
            assert!(!valid_zone(bad), "{bad}");
        }
    }

    #[test]
    fn languages_are_utf8_locales_but_c() {
        let listed = "C\nC.utf8\nPOSIX\nen_US.utf8\npt_BR.utf8\nde_DE.UTF-8\nfr_FR\n";
        assert_eq!(
            languages(listed),
            ["de_DE.UTF-8", "en_US.UTF-8", "pt_BR.UTF-8"]
        );
        assert!(!valid_language("pt_BR.utf8"));
        assert!(!valid_language("../x.UTF-8"));
    }

    #[test]
    fn the_environment_keeps_everything_but_lang() {
        assert_eq!(
            environment_with("EDITOR=vi\nLANG=en_US.UTF-8\n", "pt_BR.UTF-8"),
            "EDITOR=vi\nLANG=pt_BR.UTF-8\n"
        );
        assert_eq!(environment_with("", "pt_BR.UTF-8"), "LANG=pt_BR.UTF-8\n");
    }

    #[test]
    fn logins_are_suggested_from_the_first_name() {
        assert_eq!(suggested_login("Youri Mattar"), "youri");
        assert_eq!(suggested_login("José Silva"), "jose");
        assert_eq!(suggested_login("Øyvind Łukasz"), "oyvind");
        assert_eq!(suggested_login("1st Name"), "st");
        assert_eq!(suggested_login(""), "");
        assert!(valid_full_name("Youri T. K. K. Mattar"));
        assert!(!valid_full_name("a:b"));
    }

    #[test]
    fn the_clock_follows_the_region() {
        assert!(uses_24_hour_clock("pt_BR.UTF-8"));
        assert!(uses_24_hour_clock("en_GB.UTF-8"));
        assert!(!uses_24_hour_clock("en_US.UTF-8"));
    }

    #[test]
    fn the_keyboard_is_cosmic_comps_ron() {
        let ron = xkb_config("br", "abnt2");
        assert!(ron.contains("layout: \"br\""));
        assert!(ron.contains("variant: \"abnt2\""));
    }
}
