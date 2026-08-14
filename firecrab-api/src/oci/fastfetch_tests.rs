use super::*;

use std::io::Cursor;
use std::os::unix::fs::PermissionsExt as _;

use async_compression::tokio::bufread::GzipEncoder;
use tar::{Builder, EntryType, Header};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, BufReader};

/// `e_machine` for the architecture that is not this host's.
const FOREIGN_MACHINE: u16 = 0x00f3;

fn header(entry_type: EntryType, size: u64, mode: u32) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header
}

fn append_entry(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    entry_type: EntryType,
    data: &[u8],
    mode: u32,
) {
    let mut header = header(entry_type, data.len() as u64, mode);
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .expect("append fixture tar entry");
}

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().expect("finish fixture tar")
}

/// Builds a 64-bit little-endian ELF image with the given program headers.
fn elf(machine: u16, e_type: u16, program_headers: &[[u8; 56]], trailer: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&e_type.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&(program_headers.len() as u16).to_le_bytes());
    for entry in program_headers {
        bytes.extend_from_slice(entry);
    }
    bytes.extend_from_slice(trailer);
    bytes
}

fn program_header(p_type: u32, p_offset: u64, p_filesz: u64) -> [u8; 56] {
    let mut entry = [0_u8; 56];
    entry[..4].copy_from_slice(&p_type.to_le_bytes());
    entry[8..16].copy_from_slice(&p_offset.to_le_bytes());
    entry[32..40].copy_from_slice(&p_filesz.to_le_bytes());
    entry
}

fn host_machine() -> u16 {
    match Architecture::HOST {
        Architecture::X86_64 => 62,
        Architecture::Aarch64 => 183,
    }
}

/// Official polyfilled builds are dynamically linked; the verifier must accept that.
fn dynamic_program() -> Vec<u8> {
    let interp = b"/lib64/ld-linux-x86-64.so.2\0";
    let headers_end = 64 + 56 * 2;
    elf(
        host_machine(),
        3,
        &[
            program_header(1, 0, 0),
            program_header(3, headers_end as u64, interp.len() as u64),
        ],
        interp,
    )
}

async fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzipEncoder::new(BufReader::new(Cursor::new(bytes.to_vec())));
    let mut out = Vec::new();
    encoder
        .read_to_end(&mut out)
        .await
        .expect("gzip fixture archive");
    out
}

#[tokio::test]
async fn inspect_accepts_a_dynamically_linked_host_elf() {
    let directory = tempdir().expect("create fixture directory");
    let path = directory.path().join("fastfetch");
    let bytes = dynamic_program();
    std::fs::write(&path, &bytes).expect("write fixture");
    let program = fastfetch::inspect_fastfetch(&path, Architecture::HOST, None)
        .await
        .expect("dynamic ELF is the official shape");
    assert_eq!(program.size(), bytes.len() as u64);
    assert_eq!(program.digest(), &Sha256Digest::of_bytes(&bytes));
}

#[tokio::test]
async fn inspect_rejects_an_empty_file() {
    let directory = tempdir().expect("create fixture directory");
    let path = directory.path().join("fastfetch");
    std::fs::write(&path, []).expect("write empty fixture");
    fastfetch::inspect_fastfetch(&path, Architecture::HOST, None)
        .await
        .expect_err("empty program cannot run");
}

#[tokio::test]
async fn inspect_rejects_a_non_elf() {
    let directory = tempdir().expect("create fixture directory");
    let path = directory.path().join("fastfetch");
    std::fs::write(&path, b"#!/bin/sh\n").expect("write fixture");
    fastfetch::inspect_fastfetch(&path, Architecture::HOST, None)
        .await
        .expect_err("a script is not a guest fastfetch");
}

#[tokio::test]
async fn inspect_rejects_a_foreign_architecture() {
    let directory = tempdir().expect("create fixture directory");
    let path = directory.path().join("fastfetch");
    std::fs::write(
        &path,
        elf(FOREIGN_MACHINE, 2, &[program_header(1, 0, 0)], &[]),
    )
    .expect("write fixture");
    fastfetch::inspect_fastfetch(&path, Architecture::HOST, None)
        .await
        .expect_err("foreign ELF cannot exec in this guest");
}

#[tokio::test]
async fn inspect_rejects_a_digest_mismatch() {
    let directory = tempdir().expect("create fixture directory");
    let path = directory.path().join("fastfetch");
    std::fs::write(&path, dynamic_program()).expect("write fixture");
    let expected = "00".repeat(32);
    fastfetch::inspect_fastfetch(&path, Architecture::HOST, Some(expected.as_str()))
        .await
        .expect_err("pinned digest must match");
}

#[tokio::test]
async fn extract_lifts_the_usr_bin_fastfetch_member() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "fastfetch-linux-amd64-polyfilled/usr/bin/fastfetch",
        EntryType::Regular,
        b"fastfetch-bytes",
        0o755,
    );
    append_entry(
        &mut builder,
        "fastfetch-linux-amd64-polyfilled/usr/share/fastfetch/presets/all.jsonc",
        EntryType::Regular,
        b"{}",
        0o644,
    );
    let archive = directory.path().join("archive.tgz");
    std::fs::write(&archive, gzip(&finish(builder)).await).expect("write archive");
    let dest = directory.path().join("fastfetch");
    fastfetch::extract_member(
        &archive,
        "fastfetch-linux-amd64-polyfilled/usr/bin/fastfetch",
        &dest,
    )
    .await
    .expect("extract pinned member");
    assert_eq!(
        std::fs::read(&dest).expect("read extracted"),
        b"fastfetch-bytes"
    );
    assert_eq!(
        std::fs::metadata(&dest).expect("stat").permissions().mode() & 0o111,
        0o111
    );
}

#[tokio::test]
async fn extract_rejects_a_tarball_without_the_program() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "README", EntryType::Regular, b"hi", 0o644);
    let archive = directory.path().join("archive.tgz");
    std::fs::write(&archive, gzip(&finish(builder)).await).expect("write archive");
    let dest = directory.path().join("fastfetch");
    fastfetch::extract_member(
        &archive,
        "fastfetch-linux-amd64-polyfilled/usr/bin/fastfetch",
        &dest,
    )
    .await
    .expect_err("missing member");
    assert!(!dest.exists());
}

#[test]
fn override_is_unset_by_default() {
    // Parallel tests share the process environment; only the empty default
    // is safe to assert here. Operators set FIRECRAB_OCI_FASTFETCH_PATH.
    if std::env::var_os("FIRECRAB_OCI_FASTFETCH_PATH").is_none() {
        assert!(fastfetch::configured_override().is_none());
    }
}
