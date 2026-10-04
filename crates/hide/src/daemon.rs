//! `hide daemon`: hideupd, the D-Bus face of updates — `os.hide.Update1`
//! on the system bus. See ARCHITECTURE.md, "The desktop".
//!
//! Every operation it offers is `hide`'s own, run as a child process: the
//! same code a root shell runs, so there is one implementation of each,
//! and an operation that fails or crashes takes nothing of the daemon with
//! it. What the daemon adds is who may ask — root, or whoever polkit says
//! — and one operation at a time. The child's output comes back as
//! `Progress` signals, and its end as `Finished`.
//!
//! The `hide` command line is a client: see client.rs.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;
use zbus::{Connection, fdo, interface};

pub const NAME: &str = "os.hide.Update1";
pub const PATH: &str = "/os/hide/Update1";
/// Set for the child the daemon runs, so that it does the work rather than
/// ask the daemon to.
pub const DIRECT: &str = "HIDE_DIRECT";

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve())
}

async fn serve() -> Result<()> {
    let update = Update1 {
        busy: Arc::new(AtomicBool::new(false)),
        next_job: AtomicU32::new(1),
    };
    let _connection = zbus::connection::Builder::system()?
        .name(NAME)?
        .serve_at(PATH, update)?
        // First-boot setup, on the same name: see setup_service.rs.
        .serve_at(
            crate::setup_service::PATH,
            crate::setup_service::Setup1::default(),
        )?
        .build()
        .await
        .context("taking os.hide.Update1 on the system bus")?;
    say("serving os.hide.Update1");
    std::future::pending::<()>().await;
    Ok(())
}

struct Update1 {
    busy: Arc<AtomicBool>,
    next_job: AtomicU32,
}

#[interface(name = "os.hide.Update1")]
impl Update1 {
    /// Stage the system in an OCI image; `hide update --image IMAGE`, or with
    /// IMAGE empty, `hide update`: the channel's.
    async fn update(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        image: String,
    ) -> fdo::Result<u32> {
        authorize(connection, &header, "os.hide.update.update").await?;
        // Empty: the configured channel's image.
        let mut args = vec!["update".into()];
        if !image.is_empty() {
            args.extend(["--image".into(), image]);
        }
        self.start(emitter, args)
    }

    /// Boot the previous deployment next; `hide rollback`.
    async fn rollback(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<u32> {
        authorize(connection, &header, "os.hide.update.rollback").await?;
        self.start(emitter, vec!["rollback".into()])
    }

    /// `hide ext add IMAGE`.
    async fn add_extension(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        image: String,
    ) -> fdo::Result<u32> {
        authorize(connection, &header, "os.hide.update.extensions").await?;
        self.start(emitter, vec!["ext".into(), "add".into(), image])
    }

    /// `hide ext remove NAME`.
    async fn remove_extension(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        name: String,
    ) -> fdo::Result<u32> {
        authorize(connection, &header, "os.hide.update.extensions").await?;
        self.start(emitter, vec!["ext".into(), "remove".into(), name])
    }

    /// `hide gc`.
    async fn collect(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<u32> {
        authorize(connection, &header, "os.hide.update.update").await?;
        self.start(emitter, vec!["gc".into()])
    }

    /// What `hide status` says. Anyone may ask.
    async fn status(&self) -> fdo::Result<String> {
        self.read(&["status"]).await
    }

    /// The deployments, in the order they boot: edition, version, digest,
    /// state (good, trying, or bad), running, boots next. Anyone may ask.
    async fn deployments(&self) -> fdo::Result<Vec<(String, u64, String, String, bool, bool)>> {
        let text = self.read(&["status", "--porcelain"]).await?;
        Ok(text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                if fields.next()? != "deployment" {
                    return None;
                }
                Some((
                    fields.next()?.to_owned(),
                    fields.next()?.parse().ok()?,
                    fields.next()?.to_owned(),
                    fields.next()?.to_owned(),
                    fields.next()? == "true",
                    fields.next()? == "true",
                ))
            })
            .collect())
    }

    /// The image an update with none named takes: the registry's, for this
    /// edition and the configured channel. Empty if update.conf names none.
    /// Anyone may ask.
    async fn channel(&self) -> fdo::Result<String> {
        let text = self.read(&["status", "--porcelain"]).await?;
        Ok(text
            .lines()
            .find_map(|line| line.strip_prefix("channel\t"))
            .unwrap_or_default()
            .to_owned())
    }

    /// How the disk is protected, as `hide status` says it. Anyone may ask.
    async fn disk(&self) -> fdo::Result<String> {
        let text = self.read(&["status", "--porcelain"]).await?;
        Ok(text
            .lines()
            .find_map(|line| line.strip_prefix("disk\t"))
            .unwrap_or_default()
            .to_owned())
    }

    /// What `hide ext list` says. Anyone may ask.
    async fn extensions(&self) -> fdo::Result<String> {
        self.read(&["ext", "list"]).await
    }

    #[zbus(property)]
    async fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// A line of a job's output.
    #[zbus(signal)]
    async fn progress(emitter: &SignalEmitter<'_>, job: u32, line: &str) -> zbus::Result<()>;

    /// A job is over: whether it worked, and if not, why.
    #[zbus(signal)]
    async fn finished(
        emitter: &SignalEmitter<'_>,
        job: u32,
        ok: bool,
        error: &str,
    ) -> zbus::Result<()>;
}

impl Update1 {
    /// Starts `hide ARGS` as job N and returns N: the caller has the
    /// signals to follow it by.
    fn start(&self, emitter: SignalEmitter<'_>, args: Vec<String>) -> fdo::Result<u32> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(fdo::Error::Failed(
                "another update operation is running".into(),
            ));
        }
        let job = self.next_job.fetch_add(1, Ordering::SeqCst);
        say(&format!("job {job}: hide {}", args.join(" ")));
        let emitter = emitter.into_owned();
        let busy = Arc::clone(&self.busy);
        tokio::spawn(async move {
            let (ok, error) = match run_job(&emitter, job, &args).await {
                Ok(()) => (true, String::new()),
                Err(error) => (false, format!("{error:#}")),
            };
            say(&format!(
                "job {job}: {}",
                if ok { "done" } else { error.as_str() }
            ));
            busy.store(false, Ordering::SeqCst);
            let _ = Update1::finished(&emitter, job, ok, &error).await;
        });
        Ok(job)
    }

    async fn read(&self, args: &[&str]) -> fdo::Result<String> {
        let output = hide()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| fdo::Error::Failed(format!("running hide: {e}")))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if output.status.success() {
            Ok(text)
        } else {
            Err(fdo::Error::Failed(text.trim().to_owned()))
        }
    }
}

/// The job's output, line by line as it comes, from stdout and stderr both.
async fn run_job(emitter: &SignalEmitter<'_>, job: u32, args: &[String]) -> Result<()> {
    let mut child = hide()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running hide")?;
    let stdout = child.stdout.take().context("the job's stdout")?;
    let stderr = child.stderr.take().context("the job's stderr")?;
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();
    let mut last = String::new();
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        let line = tokio::select! {
            line = out.next_line(), if out_open => line?.or_else(|| { out_open = false; None }),
            line = err.next_line(), if err_open => line?.or_else(|| { err_open = false; None }),
        };
        if let Some(line) = line {
            let _ = Update1::progress(emitter, job, &line).await;
            last = line;
        }
    }
    let status = child.wait().await?;
    if status.success() {
        Ok(())
    } else {
        // The command line's own last word, `hide: <error>`, is the reason.
        let reason = last.strip_prefix("hide: ").unwrap_or(&last);
        anyhow::bail!("{reason}")
    }
}

fn hide() -> Command {
    let mut command = Command::new("/proc/self/exe");
    command.env(DIRECT, "1");
    command
}

/// Root may; anyone else, if polkit says so for `action`. Without polkit —
/// Minimal has none — only root.
async fn authorize(connection: &Connection, header: &Header<'_>, action: &str) -> fdo::Result<()> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("a call with no sender".into()))?
        .to_owned();
    let bus = fdo::DBusProxy::new(connection).await?;
    let uid = bus.get_connection_unix_user(sender.clone().into()).await?;
    if uid == 0 {
        return Ok(());
    }
    let subject = (
        "system-bus-name",
        HashMap::from([("name", Value::from(sender.as_str()))]),
    );
    // 1: AllowUserInteraction — polkit's agent may ask for a password.
    let reply = connection
        .call_method(
            Some("org.freedesktop.PolicyKit1"),
            "/org/freedesktop/PolicyKit1/Authority",
            Some("org.freedesktop.PolicyKit1.Authority"),
            "CheckAuthorization",
            &(subject, action, HashMap::<&str, &str>::new(), 1u32, ""),
        )
        .await;
    let authorized = match reply {
        Ok(message) => {
            let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) =
                message.body().deserialize()?;
            authorized
        }
        Err(_) => false,
    };
    if authorized {
        Ok(())
    } else {
        say(&format!("uid {uid} refused {action}"));
        Err(fdo::Error::AccessDenied(format!(
            "not authorized for {action}"
        )))
    }
}

fn say(line: &str) {
    eprintln!("hideupd: {line}");
}
