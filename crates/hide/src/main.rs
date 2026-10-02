//! hide: hideOS's command line.
//!
//! ```text
//! hide install --payload FILE --disk DEVICE [--poweroff]
//! ```
//!
//! `update`, `rollback`, `status`, `rebase` and `shell` come with H3 and H5.

#![forbid(unsafe_code)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod install;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hide: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        #[cfg(target_os = "linux")]
        Some("install") => install::run(args.get(1..).unwrap_or_default()),
        Some("help" | "--help" | "-h") | None => {
            print!("{}", USAGE);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

const USAGE: &str = "usage: hide <command>

    install --payload FILE --disk DEVICE [--poweroff]
        Install hideOS on DEVICE, erasing it, from a payload hideforge built.
";
