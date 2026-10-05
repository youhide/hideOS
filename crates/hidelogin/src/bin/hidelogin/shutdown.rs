//! hidelogin stopped — by oxinit, at a shutdown — ends the sessions. Their
//! processes are in hidelogin's slice, beside oxinit's units rather than
//! in one, so nothing else would end them before the filesystems are
//! synced: they get SIGTERM, a few seconds to save their work, then the
//! slice is killed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

/// The longest a session gets between SIGTERM and SIGKILL.
const GRACE: Duration = Duration::from_secs(5);

fn slice() -> PathBuf {
    Path::new("/sys/fs/cgroup").join(hidelogin::session::SLICE)
}

/// Every process in `dir` and the cgroups under it.
fn processes(dir: &Path, into: &mut Vec<i32>) {
    if let Ok(text) = std::fs::read_to_string(dir.join("cgroup.procs")) {
        into.extend(text.lines().filter_map(|l| l.trim().parse::<i32>().ok()));
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                processes(&entry.path(), into);
            }
        }
    }
}

fn populated() -> bool {
    std::fs::read_to_string(slice().join("cgroup.events"))
        .is_ok_and(|text| text.lines().any(|l| l == "populated 1"))
}

pub async fn end_sessions() {
    let mut pids = Vec::new();
    processes(&slice(), &mut pids);
    if pids.is_empty() {
        return;
    }
    println!("hidelogin: ending the sessions, {} process(es)", pids.len());
    for pid in pids {
        if let Some(pid) = Pid::from_raw(pid) {
            let _ = kill_process(pid, Signal::TERM);
        }
    }
    let start = Instant::now();
    while populated() && start.elapsed() < GRACE {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if populated() {
        println!("hidelogin: the sessions did not end in time; killing them");
        let _ = std::fs::write(slice().join("cgroup.kill"), "1");
    }
}
