# CLAUDE.md

Conventions for this repository. Read [ARCHITECTURE.md](ARCHITECTURE.md) before
writing code, and oxinit's
[ARCHITECTURE.md](https://github.com/youhide/oxinit/blob/main/ARCHITECTURE.md)
before writing anything on the boot path.

## What this is

hideOS is a sealed, image-based Linux workstation OS: composefs + fs-verity
system image, A/B-style deployments with automatic rollback, oxinit as PID 1,
COSMIC as the desktop. Pre-alpha — see [ROADMAP.md](ROADMAP.md).

## Where things go

- Changes to PID 1 belong in the oxinit repository, not here. ARCHITECTURE lists
  what hideOS needs from oxinit.
- Decisions marked **Proposed** in ARCHITECTURE are not settled. Do not build on
  one without settling it first.

## Rules for boot-path crates

`hidestage`, `hideboot`, and the commit step of `hideupd`. A bug here is a
machine that does not boot.

- `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]`.
  No `#[allow]` for any of them. No `todo!`, `unimplemented!`, `assert!`
  outside `#[cfg(test)]`.
- `panic = "unwind"` in every profile. Never `abort`.
- Syscalls through `rustix`. `unsafe` only in the crate's `sys` module, every
  block with a `// SAFETY:` comment.
- Errors are `thiserror` enums, one per crate. No `anyhow`.
- No async runtime.

## Rules everywhere

- MSRV is stable minus two releases. No nightly features.
- Policy logic lives in host-testable library crates with no Linux-only
  dependencies; Linux-only code stays in the binaries. Same split as oxinit.
- Defaults are read from `/usr/lib/<name>/`, overrides from `/etc/<name>/`.
  Nothing this project writes requires a file in `/etc` to exist.
- A milestone is done when it boots and is verified, as written in ROADMAP —
  not when the code compiles.
- Documentation is in English. Explain the reason for a non-obvious decision
  where the decision is made.
