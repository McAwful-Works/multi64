//! SummerCart64 **SD/FAT end-to-end** hardware test.
//!
//! Drives [`CartSession`] — the same API Xfer64's UI commands call (`crates/xfer64/src-tauri`) —
//! against a real cart over USB, so the SD stack gets exercised outside a RAM disk. **Never runs in
//! CI**: it needs an SC64 with an SD card inserted, and it writes to that card.
//!
//! This is the SD/FAT stack, not the L3 bridge (see `CLAUDE.md`, "Two independent USB stacks"). The
//! cart does **not** need to be in a console — SD access is served by the cart's own USB firmware.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release -- --port COM4
//! ```
//!
//! Everything is created under one work directory (`--dir`, default `/xfer64-e2e`) and removed at
//! the end unless `--keep` is passed. If `multi64d` holds the port, release it first
//! (`POST /v1/serial/release`) — the two stacks cannot share the COM port.

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(
    name = "sc64-sd-e2e",
    about = "SummerCart64 SD/FAT end-to-end hardware test"
)]
struct Args {
    /// Cart serial device (e.g. COM4, /dev/ttyACM0).
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    /// Cart-side working directory. Created, used, and removed by the run.
    #[arg(long, default_value = "/xfer64-e2e")]
    dir: String,

    /// Leave the working directory on the card instead of deleting it.
    #[arg(long, default_value_t = false)]
    keep: bool,

    /// Size of the large round-trip payload, in KiB. Chosen to span many SD sectors.
    #[arg(long, default_value_t = 512)]
    big_kib: usize,
}

/// One check. `run` returns `Ok(detail)` for a pass; the detail is printed beside the name.
struct Harness {
    passed: u32,
    failed: u32,
}

impl Harness {
    fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
        }
    }

    fn step<T>(&mut self, name: &str, f: impl FnOnce() -> io::Result<(T, String)>) -> Option<T> {
        print!("  {name:<44}");
        let _ = io::stdout().flush();
        let t = Instant::now();
        match f() {
            Ok((v, detail)) => {
                self.passed += 1;
                println!("PASS  {:>6}ms  {detail}", t.elapsed().as_millis());
                Some(v)
            }
            Err(e) => {
                self.failed += 1;
                println!("FAIL  {:>6}ms  {e}", t.elapsed().as_millis());
                None
            }
        }
    }
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// Deterministic non-repeating payload: a constant byte would hide sector-ordering bugs, which are
/// exactly what a block-device round trip should catch.
fn payload(len: usize, seed: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x = seed | 1;
    for _ in 0..len {
        // xorshift32
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push((x >> 24) as u8);
    }
    v
}

fn find<'a>(
    entries: &'a [multi64_sc64_sd::SessionEntry],
    name: &str,
) -> Option<&'a multi64_sc64_sd::SessionEntry> {
    entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    let tmp = std::env::temp_dir().join("sc64-sd-e2e");
    let _ = std::fs::create_dir_all(&tmp);

    println!("SummerCart64 SD/FAT end-to-end");
    println!("  port      {} @ {}", args.port, args.baud);
    println!("  cart dir  {}", args.dir);
    println!("  host tmp  {}", tmp.display());
    println!();

    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);
    println!(
        "opened session; filesystem = {}",
        if session.is_exfat() { "exFAT" } else { "FAT32" }
    );
    println!();

    let mut h = Harness::new();
    let dir = args.dir.trim_end_matches('/').to_string();
    let small = payload(4096, 0xA5A5_1234);
    let big = payload(args.big_kib * 1024, 0x5EED_C0DE);
    let small_src = tmp.join("small.bin");
    let big_src = tmp.join("big.bin");
    std::fs::write(&small_src, &small)?;
    std::fs::write(&big_src, &big)?;

    run_checks(
        &mut h, &session, &dir, &tmp, &small_src, &big_src, &small, &big,
    );

    // Cleanup runs regardless of earlier failures so a bad run does not litter the card.
    if args.keep {
        println!("\n--keep: leaving {dir} on the card");
    } else {
        println!();
        h.step("cleanup: remove work directory", || {
            session.remove_cart_path(&dir)?;
            let root = session.list_dir("/")?;
            let leaf = dir.trim_start_matches('/');
            if find(&root, leaf).is_some() {
                return Err(err(format!("{dir} still listed after delete")));
            }
            Ok(((), format!("{dir} gone")))
        });
    }

    let _ = session.close();
    println!("\n{} passed, {} failed", h.passed, h.failed);
    if h.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_checks(
    h: &mut Harness,
    session: &CartSession,
    dir: &str,
    tmp: &Path,
    small_src: &Path,
    big_src: &Path,
    small: &[u8],
    big: &[u8],
) {
    h.step("list root", || {
        let e = session.list_dir("/")?;
        Ok(((), format!("{} entries", e.len())))
    });

    h.step("mkdir work directory", || {
        session.mkdir_cart(dir)?;
        let root = session.list_dir("/")?;
        let leaf = dir.trim_start_matches('/');
        match find(&root, leaf) {
            Some(e) if e.is_dir => Ok(((), format!("{dir} created"))),
            Some(_) => Err(err(format!("{dir} exists but is not a directory"))),
            None => Err(err(format!("{dir} missing from root listing"))),
        }
    });

    h.step("mkdir nested subdirectory", || {
        let sub = format!("{dir}/nested");
        session.mkdir_cart(&sub)?;
        match session.cart_path_entry_kind(&sub)? {
            Some(true) => Ok(((), format!("{sub} is a directory"))),
            Some(false) => Err(err("nested reported as a file")),
            None => Err(err("nested not found")),
        }
    });

    h.step("upload small file (4 KiB)", || {
        session.import_from_pc_with_progress(small_src, dir, "small.bin", false, |_| true)?;
        let listed = session.list_dir(dir)?;
        let e = find(&listed, "small.bin").ok_or_else(|| err("small.bin not listed"))?;
        if e.size != small.len() as u64 {
            return Err(err(format!(
                "size mismatch: listed {}, expected {}",
                e.size,
                small.len()
            )));
        }
        Ok(((), format!("{} bytes", e.size)))
    });

    let big_bytes = big.len();
    h.step(
        &format!("upload large file ({} KiB)", big_bytes / 1024),
        || {
            // `progress` reports a **delta** per chunk, not a running total (partition.rs:291), so the
            // deltas must sum to the file size — a progress bar that drives off these is only correct
            // if they account for every byte.
            let (mut sum, mut calls) = (0u64, 0u32);
            session.import_from_pc_with_progress(big_src, dir, "big.bin", false, |n| {
                sum += n;
                calls += 1;
                true
            })?;
            let listed = session.list_dir(dir)?;
            let e = find(&listed, "big.bin").ok_or_else(|| err("big.bin not listed"))?;
            if e.size != big_bytes as u64 {
                return Err(err(format!(
                    "size mismatch: listed {}, expected {big_bytes}",
                    e.size
                )));
            }
            if sum != big_bytes as u64 {
                return Err(err(format!(
                    "progress deltas sum to {sum}, expected {big_bytes}"
                )));
            }
            Ok((
                (),
                format!("{} bytes, {calls} progress calls sum to {sum}", e.size),
            ))
        },
    );

    h.step("total_bytes_for_cart_entry (file)", || {
        let n = session.total_bytes_for_cart_entry(&format!("{dir}/big.bin"))?;
        if n != big_bytes as u64 {
            return Err(err(format!("reported {n}, expected {big_bytes}")));
        }
        Ok(((), format!("{n} bytes")))
    });

    h.step("total_bytes_for_cart_entry (directory)", || {
        let n = session.total_bytes_for_cart_entry(dir)?;
        let expect = (small.len() + big_bytes) as u64;
        if n != expect {
            return Err(err(format!("reported {n}, expected {expect} for the tree")));
        }
        Ok(((), format!("{n} bytes across the tree")))
    });

    h.step("download small file and compare bytes", || {
        let dest = tmp.join("small.roundtrip");
        let _ = std::fs::remove_file(&dest);
        session.copy_cart_entry_to_host_with_progress(
            &format!("{dir}/small.bin"),
            &dest,
            false,
            |_| true,
        )?;
        let got = std::fs::read(&dest)?;
        if got != small {
            return Err(err(format!(
                "content differs (got {} bytes, sent {})",
                got.len(),
                small.len()
            )));
        }
        Ok(((), "identical".into()))
    });

    h.step("download large file and compare bytes", || {
        let dest = tmp.join("big.roundtrip");
        let _ = std::fs::remove_file(&dest);
        let (mut sum, mut calls) = (0u64, 0u32);
        session.copy_cart_entry_to_host_with_progress(
            &format!("{dir}/big.bin"),
            &dest,
            false,
            |n| {
                sum += n;
                calls += 1;
                true
            },
        )?;
        let got = std::fs::read(&dest)?;
        if got.len() != big_bytes {
            return Err(err(format!("length {} != {big_bytes}", got.len())));
        }
        if let Some(i) = got.iter().zip(big).position(|(a, b)| a != b) {
            return Err(err(format!("first byte difference at offset {i}")));
        }
        if sum != big_bytes as u64 {
            return Err(err(format!(
                "progress deltas sum to {sum}, expected {big_bytes}"
            )));
        }
        Ok((
            (),
            format!("identical, {big_bytes} bytes, {calls} progress calls"),
        ))
    });

    h.step("cancelling an upload yields Interrupted", || {
        // Returning false from `progress` must abort. Xfer64's cancel button depends on this.
        let r =
            session.import_from_pc_with_progress(big_src, dir, "cancelled.bin", false, |_| false);
        match r {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                return Err(err(format!(
                    "aborted with {:?}, expected Interrupted",
                    e.kind()
                )))
            }
            Ok(()) => return Err(err("upload completed despite cancellation")),
        }
        // A cancelled upload must not leave a complete file behind.
        let listed = session.list_dir(dir)?;
        let left = find(&listed, "cancelled.bin").map(|e| e.size);
        if let Some(sz) = left {
            if sz >= big_bytes as u64 {
                return Err(err(format!("cancelled upload left a full {sz}-byte file")));
            }
            session.remove_cart_path(&format!("{dir}/cancelled.bin"))?;
            return Ok((
                (),
                format!("Interrupted; partial {sz}-byte file cleaned up"),
            ));
        }
        Ok(((), "Interrupted; nothing left behind".into()))
    });

    h.step("cancelling a download yields Interrupted", || {
        let dest = tmp.join("big.cancelled");
        let _ = std::fs::remove_file(&dest);
        let r = session.copy_cart_entry_to_host_with_progress(
            &format!("{dir}/big.bin"),
            &dest,
            false,
            |_| false,
        );
        match r {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                return Err(err(format!(
                    "aborted with {:?}, expected Interrupted",
                    e.kind()
                )))
            }
            Ok(()) => return Err(err("download completed despite cancellation")),
        }
        let left = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        if left >= big_bytes as u64 {
            return Err(err(format!(
                "cancelled download left a full {left}-byte file"
            )));
        }
        Ok(((), format!("Interrupted; {left} bytes on disk")))
    });

    h.step("skip_existing does not overwrite", || {
        // Upload different content under the same name with skip_existing = true.
        let other = tmp.join("other.bin");
        std::fs::write(&other, payload(4096, 0x1111_2222))?;
        session.import_from_pc_with_progress(&other, dir, "small.bin", true, |_| true)?;
        let dest = tmp.join("small.afterskip");
        let _ = std::fs::remove_file(&dest);
        session.copy_cart_entry_to_host_with_progress(
            &format!("{dir}/small.bin"),
            &dest,
            false,
            |_| true,
        )?;
        if std::fs::read(&dest)? != small {
            return Err(err("skip_existing overwrote the original"));
        }
        Ok(((), "original preserved".into()))
    });

    h.step("rename file", || {
        let from = format!("{dir}/small.bin");
        let to = format!("{dir}/renamed.bin");
        session.rename_cart(&from, &to)?;
        let listed = session.list_dir(dir)?;
        if find(&listed, "small.bin").is_some() {
            return Err(err("old name still present"));
        }
        find(&listed, "renamed.bin").ok_or_else(|| err("new name not listed"))?;
        Ok(((), "small.bin -> renamed.bin".into()))
    });

    h.step("upload into nested subdirectory", || {
        let sub = format!("{dir}/nested");
        session.import_from_pc_with_progress(small_src, &sub, "inner.bin", false, |_| true)?;
        let listed = session.list_dir(&sub)?;
        let e = find(&listed, "inner.bin").ok_or_else(|| err("inner.bin not listed"))?;
        if e.size != small.len() as u64 {
            return Err(err(format!("size {} unexpected", e.size)));
        }
        Ok(((), format!("{sub}/inner.bin, {} bytes", e.size)))
    });

    h.step("cart_path_entry_kind on a missing path", || {
        match session.cart_path_entry_kind(&format!("{dir}/does-not-exist"))? {
            None => Ok(((), "None as expected".into())),
            Some(_) => Err(err("reported a kind for a missing path")),
        }
    });

    h.step("delete single file", || {
        let p = format!("{dir}/renamed.bin");
        session.remove_cart_path(&p)?;
        let listed = session.list_dir(dir)?;
        if find(&listed, "renamed.bin").is_some() {
            return Err(err("still listed after delete"));
        }
        Ok(((), "renamed.bin removed".into()))
    });

    h.step("recursive delete of a non-empty directory", || {
        let sub = format!("{dir}/nested");
        let mut traced = Vec::new();
        session.remove_cart_path_traced(&sub, &mut |s| traced.push(s.to_string()))?;
        if session.cart_path_entry_kind(&sub)?.is_some() {
            return Err(err("nested still present"));
        }
        Ok(((), format!("{} paths traced", traced.len())))
    });

    h.step("re-list after mutations", || {
        let listed = session.list_dir(dir)?;
        let names: Vec<_> = listed.iter().map(|e| e.name.as_str()).collect();
        if names != ["big.bin"] {
            return Err(err(format!("unexpected remaining entries: {names:?}")));
        }
        Ok(((), "only big.bin remains".into()))
    });
}
