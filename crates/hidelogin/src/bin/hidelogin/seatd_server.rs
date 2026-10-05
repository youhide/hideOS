//! seatd's socket: libseat, in cosmic-comp, connects here for its devices.
//! A connection is a client of the seat; who it is — its process, user and
//! login session — comes from the kernel, not from anything it says.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result};
use hidelogin::seatd;
use hidelogin::session::session_of;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};

use crate::daemon::{SeatClient, Shared, lock};

/// Where libseat looks, as seatd's own default.
pub const SOCKET: &str = "/run/seatd.sock";

pub async fn serve(shared: Shared) -> Result<()> {
    let _ = fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET).with_context(|| format!("binding {SOCKET}"))?;
    // Anyone may connect; the seat goes only to the session on the VT
    // shown, which `Daemon::client_may_take` checks.
    fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o666))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(error) = client(shared, stream).await {
                eprintln!("hidelogin: a seat client: {error:#}");
            }
        });
    }
}

async fn client(shared: Shared, stream: UnixStream) -> Result<()> {
    let cred = stream.peer_cred()?;
    let pid = cred
        .pid()
        .and_then(|p| u32::try_from(p).ok())
        .context("the peer has no pid")?;
    let uid = cred.uid();
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    let Some(session) = session_of(&cgroup) else {
        eprintln!("hidelogin: pid {pid} asked for the seat from no session");
        return Ok(());
    };
    let std_stream = stream.into_std()?;
    let writer = std_stream.try_clone()?;
    let mut reader = UnixStream::from_std(std_stream)?;
    let id = lock(&shared).add_client(SeatClient {
        stream: writer,
        pid,
        uid,
        session,
        devices: HashMap::new(),
    });

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let ended = loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(error) => break Err(error.into()),
        };
        buffer.extend_from_slice(chunk.get(..n).unwrap_or_default());
        let mut failed = None;
        loop {
            match seatd::decode(&buffer) {
                Ok(Some((request, used))) => {
                    buffer.drain(..used);
                    lock(&shared).seatd_request(id, request);
                }
                Ok(None) => break,
                Err(error) => {
                    failed = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = failed {
            break Err(anyhow::anyhow!("pid {pid}: {error}"));
        }
    };
    lock(&shared).client_gone(id);
    ended
}
