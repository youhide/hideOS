//! Unified kernel images: the systemd EFI stub with the kernel, initrd, command
//! line and os-release added as PE sections. One signed file is the whole
//! boot, which is what lets the command line — and the image digest on it —
//! be covered by the signature.
//!
//! Built with objcopy, the way the Arch wiki's "Unified kernel image" page
//! does it by hand: each section placed after the previous one, at the stub's
//! section alignment.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

pub fn build(
    stub: &Path,
    os_release: &Path,
    cmdline: &str,
    initrd: &Path,
    kernel: &Path,
    out: &Path,
) -> Result<()> {
    let scratch = out.with_extension("cmdline");
    fs::write(&scratch, cmdline)?;

    let alignment = section_alignment(stub)?;
    let mut offset = align(stub_end(stub)?, alignment);
    let mut args: Vec<String> = Vec::new();
    // Order matters only in that .linux comes last, after everything the
    // stub reads before handing over to the kernel.
    for (section, file) in [
        (".osrel", os_release),
        (".cmdline", scratch.as_path()),
        (".initrd", initrd),
        (".linux", kernel),
    ] {
        let size = fs::metadata(file)
            .with_context(|| format!("{}", file.display()))?
            .len();
        args.push("--add-section".into());
        args.push(format!("{section}={}", file.display()));
        args.push("--set-section-flags".into());
        args.push(format!("{section}=data,readonly"));
        args.push("--change-section-vma".into());
        args.push(format!("{section}={offset:#x}"));
        offset = align(offset + size, alignment);
    }

    let status = Command::new("objcopy")
        .args(&args)
        .arg(stub)
        .arg(out)
        .status()
        .context("running objcopy")?;
    let _ = fs::remove_file(&scratch);
    if !status.success() {
        bail!("objcopy failed building {}", out.display());
    }
    Ok(())
}

/// The stub's SectionAlignment, from its PE optional header.
fn section_alignment(stub: &Path) -> Result<u64> {
    let text = objdump(stub, "-p")?;
    let value = text
        .lines()
        .find_map(|l| l.strip_prefix("SectionAlignment"))
        .ok_or_else(|| anyhow!("{} has no SectionAlignment", stub.display()))?;
    u64::from_str_radix(value.trim(), 16).context("parsing SectionAlignment")
}

/// Where the stub's last section ends, as a virtual address.
fn stub_end(stub: &Path) -> Result<u64> {
    let text = objdump(stub, "-h")?;
    let mut end = 0;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // "  Idx Name Size VMA LMA File-off Algn": section rows start with
        // their index.
        if let [index, _name, size, vma, ..] = fields.as_slice()
            && index.parse::<u32>().is_ok()
        {
            let size = u64::from_str_radix(size, 16)?;
            let vma = u64::from_str_radix(vma, 16)?;
            end = end.max(vma + size);
        }
    }
    if end == 0 {
        bail!("{} has no sections", stub.display());
    }
    Ok(end)
}

fn objdump(file: &Path, flag: &str) -> Result<String> {
    let out = Command::new("objdump")
        .arg(flag)
        .arg(file)
        .output()
        .context("running objdump")?;
    if !out.status.success() {
        bail!("objdump {flag} {} failed", file.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn align(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

#[cfg(test)]
mod tests {
    use super::align;

    #[test]
    fn sections_start_on_the_alignment() {
        assert_eq!(align(0x1000, 0x1000), 0x1000);
        assert_eq!(align(0x1001, 0x1000), 0x2000);
        assert_eq!(align(1, 0x200), 0x200);
    }
}
