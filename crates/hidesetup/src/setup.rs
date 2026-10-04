//! hideupd's `os.hide.Setup1`, on the system bus: what setup offers, and
//! each choice given to it. Errors come back as the text a person reads.

use zbus::{Connection, proxy};

use crate::Offer;

#[proxy(
    interface = "os.hide.Setup1",
    default_service = "os.hide.Update1",
    default_path = "/os/hide/Setup1"
)]
trait Setup1 {
    fn languages(&self) -> zbus::Result<Vec<String>>;
    fn layouts(&self) -> zbus::Result<Vec<(String, String)>>;
    fn zones(&self) -> zbus::Result<Vec<String>>;
    fn online(&self) -> zbus::Result<bool>;
    fn wifi_networks(&self) -> zbus::Result<Vec<(String, u32, bool)>>;
    fn disk_needs_passphrase(&self) -> zbus::Result<bool>;
    fn set_language(&self, language: &str) -> zbus::Result<()>;
    fn set_keyboard(&self, layout: &str, variant: &str) -> zbus::Result<()>;
    fn set_zone(&self, zone: &str) -> zbus::Result<()>;
    fn connect_wifi(&self, ssid: &str, password: &str) -> zbus::Result<()>;
    fn create_account(&self, full_name: &str, login: &str, password: &str) -> zbus::Result<()>;
    fn secure_disk(&self, password: &str) -> zbus::Result<String>;
    fn finish(&self) -> zbus::Result<()>;
}

pub struct Setup(Setup1Proxy<'static>);

/// A D-Bus error as a sentence: hideupd's own message when it sent one.
fn say(error: zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(_, Some(message), _) => message,
        zbus::Error::FDO(fdo) => match *fdo {
            zbus::fdo::Error::Failed(m) | zbus::fdo::Error::InvalidArgs(m) => m,
            other => other.to_string(),
        },
        other => other.to_string(),
    }
}

impl Setup {
    pub async fn connect() -> Result<Self, String> {
        let bus = Connection::system().await.map_err(say)?;
        Ok(Self(Setup1Proxy::new(&bus).await.map_err(say)?))
    }

    pub async fn offer(&self) -> Result<Offer, String> {
        Ok(Offer {
            languages: self.0.languages().await.map_err(say)?,
            layouts: self.0.layouts().await.map_err(say)?,
            zones: self.0.zones().await.map_err(say)?,
            online: self.0.online().await.map_err(say)?,
            disk_needs_passphrase: self.0.disk_needs_passphrase().await.map_err(say)?,
        })
    }

    pub async fn wifi_networks(&self) -> Result<Vec<(String, u32, bool)>, String> {
        self.0.wifi_networks().await.map_err(say)
    }

    pub async fn set_language(&self, language: &str) -> Result<(), String> {
        self.0.set_language(language).await.map_err(say)
    }

    pub async fn set_keyboard(&self, layout: &str, variant: &str) -> Result<(), String> {
        self.0.set_keyboard(layout, variant).await.map_err(say)
    }

    pub async fn set_zone(&self, zone: &str) -> Result<(), String> {
        self.0.set_zone(zone).await.map_err(say)
    }

    pub async fn connect_wifi(&self, ssid: &str, password: &str) -> Result<(), String> {
        self.0.connect_wifi(ssid, password).await.map_err(say)
    }

    pub async fn create_account(
        &self,
        full_name: &str,
        login: &str,
        password: &str,
    ) -> Result<(), String> {
        self.0
            .create_account(full_name, login, password)
            .await
            .map_err(say)
    }

    pub async fn secure_disk(&self, password: &str) -> Result<String, String> {
        self.0.secure_disk(password).await.map_err(say)
    }

    pub async fn finish(&self) -> Result<(), String> {
        self.0.finish().await.map_err(say)
    }
}
