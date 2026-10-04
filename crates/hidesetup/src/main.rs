//! hidesetup: first-boot setup, as a Mac's Setup Assistant — a page at a
//! time, the language, the keyboard, a network, the time zone, the
//! account, and the disk's passphrase and recovery key. It runs as the
//! greeter's user, in the greeter's compositor, and changes nothing
//! itself: hideupd does, on `os.hide.Setup1`. See ARCHITECTURE.md,
//! "First-boot setup".

mod setup;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{self, button, column, row, text};
use cosmic::{Application, Element, executor};

use setup::Setup;

fn main() -> cosmic::iced::Result {
    let settings = Settings::default()
        .size(cosmic::iced::Size::new(720.0, 560.0))
        // No title bar, its own or the compositor's: as a Mac's Setup
        // Assistant, it is the whole screen and cannot be closed.
        .client_decorations(true);
    cosmic::app::run::<App>(settings, ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Language,
    Keyboard,
    Network,
    Zone,
    Account,
    Disk,
    RecoveryKey,
    Done,
}

/// What hideupd said it offers, read once at the start.
#[derive(Debug, Clone, Default)]
struct Offer {
    languages: Vec<String>,
    layouts: Vec<(String, String)>,
    zones: Vec<String>,
    online: bool,
    disk_needs_passphrase: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Loaded(Result<Offer, String>),
    Networks(Result<Vec<(String, u32, bool)>, String>),
    Back,
    Continue,
    Language(String),
    Layout(String),
    Filter(String),
    Zone(String),
    Network(String),
    WifiPassword(String),
    Join,
    Joined(Result<(), String>),
    Rescan,
    FullName(String),
    Login(String),
    Password(String),
    Confirm(String),
    Done(Result<Page, String>),
    RecoveryKey(Result<String, String>),
    Finished(Result<(), String>),
}

struct App {
    core: Core,
    page: Page,
    offer: Offer,
    working: bool,
    error: Option<String>,
    language: String,
    layout: String,
    filter: String,
    zone: String,
    networks: Vec<(String, u32, bool)>,
    network: Option<String>,
    wifi_password: String,
    full_name: String,
    login: String,
    login_edited: bool,
    password: String,
    confirm: String,
    recovery_key: Option<String>,
}

impl Application for App {
    type Executor = executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "os.hide.Setup";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, _flags: ()) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;
        let app = App {
            core,
            page: Page::Language,
            offer: Offer::default(),
            working: true,
            error: None,
            language: "en_US.UTF-8".to_owned(),
            layout: "us".to_owned(),
            filter: String::new(),
            zone: "UTC".to_owned(),
            networks: Vec::new(),
            network: None,
            wifi_password: String::new(),
            full_name: String::new(),
            login: String::new(),
            login_edited: false,
            password: String::new(),
            confirm: String::new(),
            recovery_key: None,
        };
        let load = Task::perform(offer(), |offer| cosmic::Action::App(Message::Loaded(offer)));
        let fullscreen = app.core.main_window_id().map_or_else(Task::none, |id| {
            cosmic::iced::window::set_mode(id, cosmic::iced::window::Mode::Fullscreen)
        });
        (app, Task::batch([load, fullscreen]))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let before = self.page;
        let task = self.step(message);
        // The keyboard and time zone pages share the search field.
        if self.page != before {
            self.filter.clear();
        }
        task
    }

    fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::active().cosmic().spacing;
        let (title, body): (&str, Element<'_, Message>) = match self.page {
            Page::Language => ("Welcome to hideOS", self.languages_view()),
            Page::Keyboard => ("Keyboard", self.layouts_view()),
            Page::Network => ("Network", self.network_view()),
            Page::Zone => ("Time zone", self.zones_view()),
            Page::Account => ("Your account", self.account_view()),
            Page::Disk => ("Disk encryption", self.disk_view()),
            Page::RecoveryKey => ("Recovery key", self.recovery_view()),
            Page::Done => ("hideOS is ready", self.done_view()),
        };

        let mut content = column::with_capacity(5)
            .spacing(spacing.space_m)
            .max_width(560.0)
            .push(text::title2(title))
            .push(body);
        if let Some(why) = &self.error {
            content = content.push(text::body(why.as_str()).class(cosmic::theme::Text::Accent));
        }
        content = content.push(self.buttons());

        widget::container(content)
            .padding(spacing.space_xl)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into()
    }
}

impl App {
    fn step(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(Ok(offer)) => {
                self.working = false;
                self.error = None;
                if offer.languages.contains(&"en_US.UTF-8".to_owned()) {
                    self.language = "en_US.UTF-8".to_owned();
                } else if let Some(first) = offer.languages.first() {
                    self.language = first.clone();
                }
                self.offer = offer;
            }
            Message::Loaded(Err(why)) | Message::Networks(Err(why)) => {
                self.working = false;
                self.error = Some(why);
            }
            Message::Networks(Ok(networks)) => {
                self.working = false;
                self.networks = networks;
            }
            Message::Language(language) => {
                self.layout = layout_for(&language).to_owned();
                self.language = language;
            }
            Message::Layout(layout) => self.layout = layout,
            Message::Filter(filter) => self.filter = filter,
            Message::Zone(zone) => self.zone = zone,
            Message::Network(name) => {
                self.network = Some(name);
                self.wifi_password.clear();
            }
            Message::WifiPassword(password) => self.wifi_password = password,
            Message::Rescan => return self.scan(),
            Message::Join => {
                let Some(ssid) = self.network.clone() else {
                    return Task::none();
                };
                let password = self.wifi_password.clone();
                self.working = true;
                self.error = None;
                return Task::perform(
                    async move { Setup::connect().await?.connect_wifi(&ssid, &password).await },
                    |r| cosmic::Action::App(Message::Joined(r)),
                );
            }
            Message::Joined(result) => {
                self.working = false;
                match result {
                    Ok(()) => {
                        self.offer.online = true;
                        self.page = Page::Zone;
                    }
                    Err(why) => self.error = Some(why),
                }
            }
            Message::FullName(name) => {
                if !self.login_edited {
                    self.login = suggested_login(&name);
                }
                self.full_name = name;
            }
            Message::Login(login) => {
                self.login_edited = true;
                self.login = login;
            }
            Message::Password(password) => self.password = password,
            Message::Confirm(confirm) => self.confirm = confirm,
            Message::Back => {
                self.error = None;
                self.page = match self.page {
                    Page::Keyboard => Page::Language,
                    Page::Network => Page::Keyboard,
                    Page::Zone if self.offer.online && self.networks.is_empty() => Page::Keyboard,
                    Page::Zone => Page::Network,
                    Page::Account => Page::Zone,
                    other => other,
                };
            }
            Message::Continue => return self.continue_from(),
            Message::Done(result) => {
                self.working = false;
                match result {
                    Ok(next) => {
                        self.page = next;
                        if next == Page::Network {
                            return self.scan();
                        }
                    }
                    Err(why) => self.error = Some(why),
                }
            }
            Message::RecoveryKey(result) => {
                self.working = false;
                match result {
                    Ok(key) => {
                        self.recovery_key = Some(key);
                        self.page = Page::RecoveryKey;
                    }
                    Err(why) => self.error = Some(why),
                }
            }
            Message::Finished(result) => {
                self.working = false;
                match result {
                    // Setup is done; the greeter takes the screen.
                    Ok(()) => std::process::exit(0),
                    Err(why) => self.error = Some(why),
                }
            }
        }
        Task::none()
    }

    /// What happens on Continue: the page's choice is given to hideupd,
    /// and the next page follows when it is taken.
    fn continue_from(&mut self) -> Task<Message> {
        self.error = None;
        let done = |next: Page| {
            move |r: Result<(), String>| cosmic::Action::App(Message::Done(r.map(|()| next)))
        };
        match self.page {
            // hideupd did not answer at the start: ask it again.
            Page::Language if self.offer.languages.is_empty() => {
                self.working = true;
                Task::perform(offer(), |offer| cosmic::Action::App(Message::Loaded(offer)))
            }
            Page::Language => {
                let language = self.language.clone();
                self.working = true;
                Task::perform(
                    async move { Setup::connect().await?.set_language(&language).await },
                    done(Page::Keyboard),
                )
            }
            Page::Keyboard => {
                let layout = self.layout.clone();
                // A cable already connected needs no Wi-Fi page.
                let next = if self.offer.online {
                    Page::Zone
                } else {
                    Page::Network
                };
                self.working = true;
                Task::perform(
                    async move { Setup::connect().await?.set_keyboard(&layout, "").await },
                    done(next),
                )
            }
            Page::Network => {
                self.page = Page::Zone;
                Task::none()
            }
            Page::Zone => {
                let zone = self.zone.clone();
                self.working = true;
                Task::perform(
                    async move { Setup::connect().await?.set_zone(&zone).await },
                    done(Page::Account),
                )
            }
            Page::Account => {
                if let Some(why) = self.account_problem() {
                    self.error = Some(why.to_owned());
                    return Task::none();
                }
                let (name, login, password) = (
                    self.full_name.clone(),
                    self.login.clone(),
                    self.password.clone(),
                );
                let next = if self.offer.disk_needs_passphrase {
                    Page::Disk
                } else {
                    Page::Done
                };
                self.working = true;
                Task::perform(
                    async move {
                        Setup::connect()
                            .await?
                            .create_account(&name, &login, &password)
                            .await
                    },
                    done(next),
                )
            }
            Page::Disk => {
                let password = self.password.clone();
                self.working = true;
                Task::perform(
                    async move { Setup::connect().await?.secure_disk(&password).await },
                    |r| cosmic::Action::App(Message::RecoveryKey(r)),
                )
            }
            Page::RecoveryKey => {
                self.page = Page::Done;
                Task::none()
            }
            Page::Done => {
                self.working = true;
                Task::perform(async { Setup::connect().await?.finish().await }, |r| {
                    cosmic::Action::App(Message::Finished(r))
                })
            }
        }
    }

    fn scan(&mut self) -> Task<Message> {
        self.working = true;
        Task::perform(
            async { Setup::connect().await?.wifi_networks().await },
            |r| cosmic::Action::App(Message::Networks(r)),
        )
    }

    /// Why the account cannot be made as typed, if it cannot.
    fn account_problem(&self) -> Option<&'static str> {
        if self.full_name.trim().is_empty() || self.full_name.contains([':', ',']) {
            Some("Type your name, without `:` or `,`.")
        } else if !valid_login(&self.login) {
            Some("The account name is lowercase letters, digits, - and _, starting with a letter.")
        } else if self.password.is_empty() {
            Some("Choose a password.")
        } else if self.password != self.confirm {
            Some("The two passwords are not the same.")
        } else {
            None
        }
    }

    fn buttons(&self) -> Element<'_, Message> {
        let back = !matches!(
            self.page,
            Page::Language | Page::Disk | Page::RecoveryKey | Page::Done
        );
        let label = match self.page {
            Page::Disk => "Encrypt with my password",
            Page::RecoveryKey => "I wrote it down",
            Page::Done => "Start using hideOS",
            Page::Network if !self.offer.online => "Skip",
            _ => "Continue",
        };
        let mut buttons = row::with_capacity(3).spacing(12).align_y(Alignment::Center);
        if back {
            buttons = buttons.push(
                button::standard("Back").on_press_maybe((!self.working).then_some(Message::Back)),
            );
        }
        buttons = buttons.push(widget::space::horizontal());
        if self.working {
            buttons = buttons.push(text::body("Working…"));
        }
        buttons
            .push(
                button::suggested(label)
                    .on_press_maybe((!self.working).then_some(Message::Continue)),
            )
            .into()
    }

    fn languages_view(&self) -> Element<'_, Message> {
        let choices = self.offer.languages.iter().map(|language| {
            (
                language.clone(),
                language_name(language).to_owned(),
                *language == self.language,
            )
        });
        column::with_capacity(2)
            .spacing(12)
            .push(text::body("Choose your language."))
            .push(choice_list(choices, Message::Language))
            .into()
    }

    fn layouts_view(&self) -> Element<'_, Message> {
        let filter = self.filter.to_lowercase();
        let choices = self
            .offer
            .layouts
            .iter()
            .filter(|(name, description)| {
                filter.is_empty()
                    || description.to_lowercase().contains(&filter)
                    || name.contains(&filter)
                    || *name == self.layout
            })
            .map(|(name, description)| (name.clone(), description.clone(), *name == self.layout));
        column::with_capacity(3)
            .spacing(12)
            .push(text::body("Choose your keyboard layout."))
            .push(widget::search_input("Search", &self.filter).on_input(Message::Filter))
            .push(choice_list(choices, Message::Layout))
            .into()
    }

    fn network_view(&self) -> Element<'_, Message> {
        let mut page = column::with_capacity(4).spacing(12);
        if self.offer.online {
            return page.push(text::body("This computer is connected.")).into();
        }
        page = page.push(text::body(
            "Choose a Wi-Fi network, or skip this and connect later in Settings.",
        ));
        let choices = self.networks.iter().map(|(ssid, signal, secured)| {
            let label = if *secured {
                format!("{ssid}  ·  secured  ·  {signal}%")
            } else {
                format!("{ssid}  ·  {signal}%")
            };
            (ssid.clone(), label, self.network.as_deref() == Some(ssid))
        });
        page = page.push(choice_list(choices, Message::Network));
        if let Some(ssid) = &self.network {
            let secured = self
                .networks
                .iter()
                .any(|(name, _, secured)| name == ssid && *secured);
            if secured {
                page = page.push(
                    widget::secure_input("Password", &self.wifi_password, None, true)
                        .on_input(Message::WifiPassword)
                        .on_submit(|_| Message::Join),
                );
            }
            page = page.push(
                row::with_capacity(2)
                    .spacing(12)
                    .push(
                        button::standard("Join")
                            .on_press_maybe((!self.working).then_some(Message::Join)),
                    )
                    .push(button::text("Look again").on_press(Message::Rescan)),
            );
        }
        page.into()
    }

    fn zones_view(&self) -> Element<'_, Message> {
        let filter = self.filter.to_lowercase().replace(' ', "_");
        let choices = self
            .offer
            .zones
            .iter()
            .filter(|zone| {
                filter.is_empty() || zone.to_lowercase().contains(&filter) || **zone == self.zone
            })
            .take(200)
            .map(|zone| (zone.clone(), zone.replace('_', " "), *zone == self.zone));
        column::with_capacity(3)
            .spacing(12)
            .push(text::body("Choose your time zone: type your city."))
            .push(widget::search_input("Search", &self.filter).on_input(Message::Filter))
            .push(choice_list(choices, Message::Zone))
            .into()
    }

    fn account_view(&self) -> Element<'_, Message> {
        column::with_capacity(5)
            .spacing(12)
            .push(text::body(if self.offer.disk_needs_passphrase {
                "Your account administers this computer. Its password also unlocks the disk."
            } else {
                "Your account administers this computer."
            }))
            .push(widget::text_input("Full name", &self.full_name).on_input(Message::FullName))
            .push(widget::text_input("Account name", &self.login).on_input(Message::Login))
            .push(
                widget::secure_input("Password", &self.password, None, true)
                    .on_input(Message::Password),
            )
            .push(
                widget::secure_input("Password, again", &self.confirm, None, true)
                    .on_input(Message::Confirm)
                    .on_submit(|_| Message::Continue),
            )
            .into()
    }

    fn disk_view(&self) -> Element<'_, Message> {
        text::body(
            "The disk is encrypted. Your password unlocks it when the computer \
             cannot unlock it by itself, and a recovery key unlocks it if the \
             password is lost.",
        )
        .into()
    }

    fn recovery_view(&self) -> Element<'_, Message> {
        column::with_capacity(3)
            .spacing(12)
            .push(text::body(
                "This key unlocks the disk if your password is lost. It is shown once: \
                 write it down and keep it away from this computer.",
            ))
            .push(text::title3(
                self.recovery_key.as_deref().unwrap_or_default(),
            ))
            .into()
    }

    fn done_view(&self) -> Element<'_, Message> {
        text::body(format!(
            "Log in as {} to start.",
            if self.full_name.is_empty() {
                &self.login
            } else {
                &self.full_name
            }
        ))
        .into()
    }
}

/// A list of choices, the chosen one marked, in a scrollable box.
fn choice_list<'a>(
    choices: impl Iterator<Item = (String, String, bool)>,
    on_choose: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    let mut list = widget::list_column();
    for (value, label, chosen) in choices {
        let mut line = row::with_capacity(2)
            .spacing(12)
            .align_y(Alignment::Center)
            .push(text::body(label))
            .push(widget::space::horizontal());
        if chosen {
            line = line.push(widget::icon::from_name("object-select-symbolic").size(16));
        }
        list = list.add(
            button::custom(line)
                .class(cosmic::theme::Button::MenuItem)
                .width(Length::Fill)
                .on_press(on_choose(value)),
        );
    }
    widget::scrollable(list).height(Length::Fixed(260.0)).into()
}

/// What setup offers, from hideupd. hidesetup can start before hideupd has
/// taken its name on the bus, so it asks again for a while.
async fn offer() -> Result<Offer, String> {
    let mut tries = 0;
    loop {
        match async { Setup::connect().await?.offer().await }.await {
            Ok(mut offer) => {
                offer
                    .languages
                    .sort_by_key(|language| language_name(language).to_owned());
                return Ok(offer);
            }
            Err(why) if tries >= 30 => return Err(why),
            Err(_) => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

/// A layout that suits a language, before the person picks one.
fn layout_for(language: &str) -> &'static str {
    match language.split('.').next().unwrap_or_default() {
        "pt_BR" => "br",
        "pt_PT" => "pt",
        "en_GB" => "gb",
        "en_CA" | "fr_CA" => "ca",
        "de_DE" | "de_AT" => "de",
        "de_CH" => "ch",
        "fr_FR" => "fr",
        "es_ES" => "es",
        "es_AR" | "es_MX" => "latam",
        "it_IT" => "it",
        "cs_CZ" => "cz",
        "da_DK" => "dk",
        "el_GR" => "gr",
        "fi_FI" => "fi",
        "hu_HU" => "hu",
        "nb_NO" => "no",
        "nl_NL" => "nl",
        "pl_PL" => "pl",
        "ro_RO" => "ro",
        "ru_RU" => "ru",
        "sv_SE" => "se",
        "tr_TR" => "tr",
        "uk_UA" => "ua",
        _ => "us",
    }
}

/// A language's own name, for the ones hideOS carries; the locale's name
/// for the rest.
fn language_name(language: &str) -> &str {
    match language.split('.').next().unwrap_or_default() {
        "cs_CZ" => "Čeština",
        "da_DK" => "Dansk",
        "de_AT" => "Deutsch (Österreich)",
        "de_CH" => "Deutsch (Schweiz)",
        "de_DE" => "Deutsch (Deutschland)",
        "el_GR" => "Ελληνικά",
        "en_AU" => "English (Australia)",
        "en_CA" => "English (Canada)",
        "en_GB" => "English (United Kingdom)",
        "en_US" => "English (United States)",
        "es_AR" => "Español (Argentina)",
        "es_ES" => "Español (España)",
        "es_MX" => "Español (México)",
        "fi_FI" => "Suomi",
        "fr_CA" => "Français (Canada)",
        "fr_FR" => "Français (France)",
        "hu_HU" => "Magyar",
        "it_IT" => "Italiano",
        "nb_NO" => "Norsk bokmål",
        "nl_NL" => "Nederlands",
        "pl_PL" => "Polski",
        "pt_BR" => "Português (Brasil)",
        "pt_PT" => "Português (Portugal)",
        "ro_RO" => "Română",
        "ru_RU" => "Русский",
        "sv_SE" => "Svenska",
        "tr_TR" => "Türkçe",
        "uk_UA" => "Українська",
        _ => language,
    }
}

fn valid_login(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
}

/// The first name, lowercased, with what a login cannot hold left out.
fn suggested_login(full_name: &str) -> String {
    let first = full_name.split_whitespace().next().unwrap_or_default();
    let login: String = first
        .chars()
        .filter_map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-' | '_') => Some(c),
            _ => None,
        })
        .skip_while(|c| !c.is_ascii_lowercase())
        .take(32)
        .collect();
    login
}
