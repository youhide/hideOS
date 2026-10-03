//! The encrypted root: opening LUKS2 in hidestage, and the TPM2 that opens
//! it without a passphrase. Host-testable, like every policy crate here;
//! the device-mapper and TPM device calls are the callers', on Linux.
//!
//! On the boot path, so the boot path's rules: no panics, errors as values.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![forbid(unsafe_code)]

pub mod luks2;
pub mod signature;
pub mod tpm2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a LUKS2 volume")]
    NotLuks,
    #[error("the LUKS2 header is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("the LUKS2 metadata does not parse: {0}")]
    Json(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("no keyslot opens with that passphrase")]
    WrongPassphrase,
    #[error("reading the volume: {0}")]
    Io(#[from] std::io::Error),
    #[error("TPM: {0}")]
    Tpm(String),
    #[error("TPM response code {0:#x}")]
    TpmCode(u32),
}
