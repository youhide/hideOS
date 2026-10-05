//! What hidelogin does with the power key and the lid, from
//! `/usr/lib/hidelogin/logind.conf`, then `/etc/hidelogin/logind.conf` over
//! it. Neither file has to exist: the defaults are hideOS's.

use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfError {
    #[error("line {0}: not `key = value`")]
    Syntax(usize),
    #[error("line {line}: `{value}` is not something {key} can do")]
    Action {
        line: usize,
        key: String,
        value: String,
    },
    #[error("line {line}: `{value}` is not a number of seconds")]
    Seconds { line: usize, value: String },
    #[error("line {line}: no setting `{key}`")]
    Key { line: usize, key: String },
}

/// What a key or a switch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Ignore,
    Suspend,
    Hibernate,
    PowerOff,
    Reboot,
    Lock,
}

impl Action {
    fn parse(text: &str) -> Option<Action> {
        Some(match text {
            "ignore" => Action::Ignore,
            "suspend" => Action::Suspend,
            "hibernate" => Action::Hibernate,
            "poweroff" => Action::PowerOff,
            "reboot" => Action::Reboot,
            "lock" => Action::Lock,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The power key is COSMIC's, which asks: hidelogin leaves it alone.
    pub power_key: Action,
    pub lid_switch: Action,
    /// The lid closed with an external display, which COSMIC also takes
    /// for itself with a block inhibitor while it uses one.
    pub lid_switch_docked: Action,
    /// The longest a delay inhibitor holds sleep or shutdown back.
    pub inhibit_delay_max: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            power_key: Action::Ignore,
            lid_switch: Action::Suspend,
            lid_switch_docked: Action::Ignore,
            inhibit_delay_max: Duration::from_secs(5),
        }
    }
}

impl Config {
    /// The defaults with each of `texts` applied over them, in order. An
    /// INI section header, as logind's `[Login]`, is accepted and ignored.
    pub fn read(texts: &[&str]) -> Result<Config, ConfError> {
        let mut config = Config::default();
        for text in texts {
            for (n, raw) in text.lines().enumerate() {
                let line = n + 1;
                let content = raw.split('#').next().unwrap_or("").trim();
                if content.is_empty() || (content.starts_with('[') && content.ends_with(']')) {
                    continue;
                }
                let (key, value) = content.split_once('=').ok_or(ConfError::Syntax(line))?;
                let (key, value) = (key.trim(), value.trim());
                let action = || {
                    Action::parse(value).ok_or_else(|| ConfError::Action {
                        line,
                        key: key.to_owned(),
                        value: value.to_owned(),
                    })
                };
                match key {
                    "HandlePowerKey" => config.power_key = action()?,
                    "HandleLidSwitch" => config.lid_switch = action()?,
                    "HandleLidSwitchDocked" => config.lid_switch_docked = action()?,
                    "InhibitDelayMaxSec" => {
                        let seconds: u64 = value.parse().map_err(|_| ConfError::Seconds {
                            line,
                            value: value.to_owned(),
                        })?;
                        config.inhibit_delay_max = Duration::from_secs(seconds);
                    }
                    other => {
                        return Err(ConfError::Key {
                            line,
                            key: other.to_owned(),
                        });
                    }
                }
            }
        }
        Ok(config)
    }

    /// What to do when `button` is pressed: nothing while a session holds
    /// it with a block inhibitor — COSMIC takes the lid while it shows on
    /// an external display — and otherwise what the configuration says.
    pub fn action_for(&self, button: Button, docked: bool, inhibited: bool) -> Action {
        if inhibited {
            return Action::Ignore;
        }
        match button {
            Button::PowerKey => self.power_key,
            Button::LidClosed if docked => self.lid_switch_docked,
            Button::LidClosed => self.lid_switch,
        }
    }
}

/// What the machine's own buttons do, as logind names their inhibitors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    PowerKey,
    LidClosed,
}

impl Button {
    /// The inhibitor that holds it back: `handle-power-key`, ….
    pub fn inhibitor(self) -> &'static str {
        match self {
            Button::PowerKey => "handle-power-key",
            Button::LidClosed => "handle-lid-switch",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_file_the_power_key_is_cosmics_and_the_lid_suspends() {
        let config = Config::read(&[]).unwrap();
        assert_eq!(config.power_key, Action::Ignore);
        assert_eq!(config.lid_switch, Action::Suspend);
        assert_eq!(config.inhibit_delay_max, Duration::from_secs(5));
    }

    #[test]
    fn the_lid_suspends_unless_docked_or_held() {
        let config = Config::default();
        assert_eq!(
            config.action_for(Button::LidClosed, false, false),
            Action::Suspend
        );
        assert_eq!(
            config.action_for(Button::LidClosed, true, false),
            Action::Ignore
        );
        assert_eq!(
            config.action_for(Button::LidClosed, false, true),
            Action::Ignore
        );
        assert_eq!(
            config.action_for(Button::PowerKey, false, false),
            Action::Ignore
        );
        assert_eq!(Button::PowerKey.inhibitor(), "handle-power-key");
    }

    #[test]
    fn etc_goes_over_usr() {
        let usr = "# hideOS\n[Login]\nHandleLidSwitch = suspend\n";
        let etc = "HandleLidSwitch=ignore  # on a desk\nInhibitDelayMaxSec=10\n";
        let config = Config::read(&[usr, etc]).unwrap();
        assert_eq!(config.lid_switch, Action::Ignore);
        assert_eq!(config.inhibit_delay_max, Duration::from_secs(10));
    }

    #[test]
    fn a_mistake_is_said_with_its_line() {
        assert_eq!(
            Config::read(&["\nHandlePowerKey"]),
            Err(ConfError::Syntax(2))
        );
        assert!(matches!(
            Config::read(&["HandlePowerKey=explode"]),
            Err(ConfError::Action { line: 1, .. })
        ));
        assert!(matches!(
            Config::read(&["KillUserProcesses=yes"]),
            Err(ConfError::Key { .. })
        ));
        assert!(matches!(
            Config::read(&["InhibitDelayMaxSec=soon"]),
            Err(ConfError::Seconds { .. })
        ));
    }
}
