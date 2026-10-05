//! The PAM module's socket. `pam_hidelogin.so`, in the process that opens a
//! session — greetd's worker, as root — sends what the session is and gets
//! its id and runtime directory back; the connection then stays open as long
//! as that process lives. When it closes, the session has ended.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result, bail};
use hidelogin::session::parse_request;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::daemon::{Shared, lock};
use crate::login1;

pub const SOCKET: &str = "/run/hidelogin/pam.sock";

pub async fn serve(shared: Shared) -> Result<()> {
    fs::create_dir_all("/run/hidelogin")?;
    let _ = fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET).with_context(|| format!("binding {SOCKET}"))?;
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(error) = connection(shared, stream).await {
                eprintln!("hidelogin: a PAM session: {error:#}");
            }
        });
    }
}

async fn connection(shared: Shared, mut stream: UnixStream) -> Result<()> {
    if stream.peer_cred()?.uid() != 0 {
        bail!("only root opens sessions");
    }
    // The request ends with an empty line.
    let mut text = Vec::new();
    let mut byte = [0u8; 1];
    while !text.ends_with(b"\n\n") {
        if stream.read(&mut byte).await? == 0 || text.len() > 4096 {
            bail!("the request ended early");
        }
        text.extend_from_slice(&byte);
    }
    let request = parse_request(&String::from_utf8_lossy(&text))?;
    let gid = primary_gid(&request.user)?;
    if request.uid != uid_of(&request.user)? {
        bail!("{} is not uid {}", request.user, request.uid);
    }
    let session = {
        let mut daemon = lock(&shared);
        daemon.open_session(request, gid)
    };
    let session = match session {
        Ok(session) => session,
        Err(error) => {
            stream
                .write_all(format!("ERROR={error:#}\n\n").as_bytes())
                .await?;
            return Err(error);
        }
    };
    println!(
        "hidelogin: session {} opened: {} ({}), {} on {}",
        session.id,
        session.user,
        session.class.as_str(),
        session.kind.as_str(),
        session.tty
    );
    login1::session_added(&shared, &session.id).await;
    stream
        .write_all(
            format!(
                "ID={}\nRUNTIME=/run/user/{}\nSEAT={}\nVTNR={}\n\n",
                session.id,
                session.uid,
                session.seat.as_deref().unwrap_or(""),
                session.vt.map(|vt| vt.to_string()).unwrap_or_default(),
            )
            .as_bytes(),
        )
        .await?;
    // Nothing more comes; the end of the connection is the end of the
    // session.
    let mut rest = [0u8; 64];
    while stream.read(&mut rest).await.unwrap_or(0) != 0 {}
    lock(&shared).close_session(&session.id);
    login1::session_removed(&shared, &session.id).await;
    println!("hidelogin: session {} closed", session.id);
    Ok(())
}

fn passwd_entry(user: &str) -> Result<Vec<String>> {
    let passwd = fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    passwd
        .lines()
        .map(|line| line.split(':').map(str::to_owned).collect::<Vec<_>>())
        .find(|fields| fields.first().map(String::as_str) == Some(user))
        .with_context(|| format!("no user {user} in /etc/passwd"))
}

fn uid_of(user: &str) -> Result<u32> {
    let fields = passwd_entry(user)?;
    Ok(fields.get(2).context("no uid")?.parse()?)
}

fn primary_gid(user: &str) -> Result<u32> {
    let fields = passwd_entry(user)?;
    Ok(fields.get(3).context("no gid")?.parse()?)
}
