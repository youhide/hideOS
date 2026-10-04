//! The command line as a client of hideupd: `hide update`, `rollback`,
//! `ext add|remove` and `gc` ask `os.hide.Update1` to do the work, and
//! print what it says as it says it. See daemon.rs.
//!
//! Where there is no daemon — the installer, the recovery system, a boot
//! that has not reached it, a system bus that is down — the command does
//! the work itself, with the same code the daemon would have run.

use anyhow::{Result, bail};
use futures_util::StreamExt;

use crate::daemon::{DIRECT, NAME};

#[zbus::proxy(
    interface = "os.hide.Update1",
    default_service = "os.hide.Update1",
    default_path = "/os/hide/Update1"
)]
trait Update1 {
    fn update(&self, image: &str) -> zbus::Result<u32>;
    fn rollback(&self) -> zbus::Result<u32>;
    fn add_extension(&self, image: &str) -> zbus::Result<u32>;
    fn remove_extension(&self, name: &str) -> zbus::Result<u32>;
    fn collect(&self) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn progress(&self, job: u32, line: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn finished(&self, job: u32, ok: bool, error: String) -> zbus::Result<()>;
}

/// An operation the daemon offers.
pub enum Operation<'a> {
    Update(&'a str),
    Rollback,
    AddExtension(&'a str),
    RemoveExtension(&'a str),
    Collect,
}

/// Runs `operation` through hideupd. `None` when there is no hideupd to
/// ask, and the caller does it itself.
pub fn through_daemon(operation: Operation<'_>) -> Option<Result<()>> {
    if std::env::var_os(DIRECT).is_some() {
        return None;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let connection = zbus::Connection::system().await.ok()?;
        let bus = zbus::fdo::DBusProxy::new(&connection).await.ok()?;
        let name = zbus::names::BusName::try_from(NAME).ok()?;
        if !bus.name_has_owner(name).await.ok()? {
            return None;
        }
        Some(run(&connection, operation).await)
    })
}

async fn run(connection: &zbus::Connection, operation: Operation<'_>) -> Result<()> {
    let proxy = Update1Proxy::new(connection).await?;
    // Listening before asking: a job can finish before the call returns.
    let mut progress = proxy.receive_progress().await?;
    let mut finished = proxy.receive_finished().await?;
    let job = match operation {
        Operation::Update(image) => proxy.update(image).await,
        Operation::Rollback => proxy.rollback().await,
        Operation::AddExtension(image) => proxy.add_extension(image).await,
        Operation::RemoveExtension(name) => proxy.remove_extension(name).await,
        Operation::Collect => proxy.collect().await,
    }
    .map_err(refusal)?;
    loop {
        // biased: every line of a job comes before its end, and is printed
        // before it.
        tokio::select! {
            biased;
            Some(signal) = progress.next() => {
                let args = signal.args()?;
                if args.job == job {
                    eprintln!("{}", args.line);
                }
            }
            Some(signal) = finished.next() => {
                let args = signal.args()?;
                if args.job != job {
                    continue;
                }
                if args.ok {
                    return Ok(());
                }
                bail!("{}", args.error);
            }
            else => bail!("hideupd went away during the operation"),
        }
    }
}

/// A refusal, said as one: D-Bus's error name is not for people.
fn refusal(error: zbus::Error) -> anyhow::Error {
    match error {
        zbus::Error::MethodError(_, Some(text), _) => anyhow::anyhow!("{text}"),
        other => anyhow::anyhow!("asking hideupd: {other}"),
    }
}
