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
//! (`POST /v1/serial/release`) and resume it afterwards (`POST /v1/serial/resume`) — the two stacks
//! cannot share the COM port.
//!
//! Four modes exit before that suite, so they can be pointed at a card holding real data.
//! `--list` and `--verify` are read-only; `--upload` writes exactly the one file it is given, and
//! `--rm` deletes exactly the files it is named:
//!
//! ```sh
//! ROM=n64/test-rom/multi64_test.z64
//! cargo run -p sc64-sd-e2e --release -- --port COM4 --upload $ROM --to /
//! cargo run -p sc64-sd-e2e --release -- --port COM4 --list
//! cargo run -p sc64-sd-e2e --release -- --port COM4 --verify /multi64_test.z64 --against $ROM
//! cargo run -p sc64-sd-e2e --release -- --port COM4 --rm /multi64_test.z64
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
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

    /// Instead of running the test suite, list the cart's root directory and exit. Read-only.
    #[arg(long, default_value_t = false)]
    list: bool,

    /// Instead of running the test suite, read this cart path back and compare it byte-for-byte
    /// against `--against`. Read-only. Use after an upload — including `--upload` — to confirm what
    /// actually landed.
    #[arg(long, value_name = "CART_PATH")]
    verify: Option<String>,

    /// Host file that `--verify` compares against.
    #[arg(long, value_name = "HOST_PATH", requires = "verify")]
    against: Option<PathBuf>,

    /// Instead of running the test suite, copy this host file onto the card and exit. Overwrites an
    /// existing file of the same name, reporting what it replaced. Pairs with `--verify`, which
    /// reads the bytes back off the card.
    #[arg(
        long,
        value_name = "HOST_PATH",
        requires = "to",
        conflicts_with_all = ["list", "verify"]
    )]
    upload: Option<PathBuf>,

    /// Cart directory `--upload` writes into, e.g. `/` or `/roms`. Must already exist.
    #[arg(long, value_name = "CART_PARENT", requires = "upload")]
    to: Option<String>,

    /// Name to write as on the cart. Defaults to the host file's own name.
    #[arg(long = "as", value_name = "DEST_NAME", requires = "upload")]
    dest_name: Option<String>,

    /// Instead of running the test suite, delete these cart files and exit. Repeatable. Refuses
    /// directories — removing a tree off the card should not be one flag away in a test tool.
    #[arg(
        long,
        value_name = "CART_PATH",
        conflicts_with_all = ["list", "verify", "upload"]
    )]
    rm: Vec<String>,
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

    let outcome = run(&args, &session, &tmp);

    // An open USB SD session keeps the card locked away from the N64: the console refuses to boot
    // with "SD card is locked by the PC side" until it is released. `Sc64SdSession`'s `Drop`
    // guarantees the release happens; closing here is what lets a failed release be reported.
    if let Err(e) = session.close() {
        eprintln!("warning: releasing the cart SD session failed: {e}");
        eprintln!("         the console may not boot from the card until a session closes cleanly");
    }
    drop(session);

    match outcome {
        Ok(0) => Ok(()),
        Ok(code) => std::process::exit(code),
        Err(e) => Err(e),
    }
}

/// Runs the selected mode and returns the process exit code.
///
/// Nothing below here may call [`std::process::exit`]: it runs no destructors, so it would skip
/// both the explicit close and `Drop` and leave the card locked to the PC. Return a code instead —
/// `main` exits only after the session has been released.
fn run(args: &Args, session: &CartSession, tmp: &Path) -> io::Result<i32> {
    // Read-only modes: inspect the card without writing to it, so they are safe to point at a
    // cart holding real data. Both exit before the suite, which does write.
    if args.list {
        list_only(session)?;
        return Ok(0);
    }
    if let Some(cart_path) = args.verify.as_deref() {
        let host = args
            .against
            .as_deref()
            .expect("clap `requires` guarantees --against");
        return verify_only(session, cart_path, host);
    }
    // Writes one file and nothing else — also safe on a card holding real data, and likewise exits
    // before the suite.
    if let Some(src) = args.upload.as_deref() {
        let parent = args.to.as_deref().expect("clap `requires` guarantees --to");
        upload_only(session, src, parent, args.dest_name.as_deref(), &args.port)?;
        return Ok(0);
    }
    // Deletes exactly what it is named and nothing else, and likewise exits before the suite.
    if !args.rm.is_empty() {
        return rm_only(session, &args.rm);
    }

    let mut h = Harness::new();
    let dir = args.dir.trim_end_matches('/').to_string();
    let small = payload(4096, 0xA5A5_1234);
    let big = payload(args.big_kib * 1024, 0x5EED_C0DE);
    let small_src = tmp.join("small.bin");
    let big_src = tmp.join("big.bin");
    std::fs::write(&small_src, &small)?;
    std::fs::write(&big_src, &big)?;

    run_checks(
        &mut h, session, &dir, tmp, &small_src, &big_src, &small, &big,
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

    println!("\n{} passed, {} failed", h.passed, h.failed);
    Ok(if h.failed > 0 { 1 } else { 0 })
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

/// `--list`: print the cart root. Read-only.
fn list_only(session: &CartSession) -> io::Result<()> {
    let entries = session.list_dir("/")?;
    if entries.is_empty() {
        println!("/ is empty");
        return Ok(());
    }
    println!("{:<40} {:>12}  kind", "name", "size");
    for e in &entries {
        println!(
            "{:<40} {:>12}  {}{}",
            e.name,
            if e.is_dir {
                "-".to_string()
            } else {
                e.size.to_string()
            },
            if e.is_dir { "dir" } else { "file" },
            if e.hidden { " (hidden)" } else { "" }
        );
    }
    println!("\n{} entries", entries.len());
    Ok(())
}

/// Join a cart parent directory and a leaf the way [`CartSession::import_from_pc_with_progress`]
/// does (`partition.rs`, `cart_parent_trimmed`), so what is printed is the path actually written.
fn cart_join(parent: &str, name: &str) -> String {
    let base = parent.replace('\\', "/");
    let base = base.trim().trim_matches('/');
    if base.is_empty() {
        format!("/{name}")
    } else {
        format!("/{base}/{name}")
    }
}

/// `--rm`: delete the named cart files and exit, without running the suite.
///
/// Destructive on a card that may hold real data, so each path is described before it goes and
/// confirmed absent afterwards — `remove_cart_path` returning `Ok` only means the call succeeded.
/// Directories are refused: a recursive delete of a card directory is not something this tool
/// should make one flag away. Carries on past a failure so one bad path does not strand the rest,
/// and reports a non-zero exit if any failed.
fn rm_only(session: &CartSession, paths: &[String]) -> io::Result<i32> {
    let mut failed = 0u32;
    let mut removed = 0u32;
    for path in paths {
        match session.cart_path_entry_kind(path)? {
            None => println!("  {path}: not on the card, nothing to do"),
            Some(true) => {
                eprintln!("  {path}: FAIL, is a directory (remove it with Xfer64)");
                failed += 1;
            }
            Some(false) => {
                let size = session.total_bytes_for_cart_entry(path)?;
                print!("  {path}: deleting {size} bytes ... ");
                let _ = io::stdout().flush();
                if let Err(e) = session.remove_cart_path(path) {
                    println!("FAIL: {e}");
                    failed += 1;
                    continue;
                }
                match session.cart_path_entry_kind(path)? {
                    None => {
                        println!("gone");
                        removed += 1;
                    }
                    Some(_) => {
                        println!("FAIL: still listed after delete");
                        failed += 1;
                    }
                }
            }
        }
    }
    println!("\n{removed} removed, {failed} failed");
    Ok(if failed > 0 { 1 } else { 0 })
}

/// Clean up after an upload that stopped part-way, and say what was done about it.
///
/// The write loop in `multi64-sc64-sd` returns without removing what it already wrote, so a failed
/// upload leaves a file behind either way. What it looks like depends on where the write died: a
/// cancel with the link still up leaves a genuinely partial size (what the suite's cancel check
/// asserts), while a link that dies mid-write leaves the directory entry reading **0 bytes**,
/// because the size is only written when the filesystem flushes — observed by pulling USB during
/// a 16 MiB `--upload`. Neither is safe to leave sitting on the card.
///
/// Mirrors `cleanup_partial_import` in `crates/xfer64/src-tauri/src/cart_serial_sd.rs`; only the
/// destination of the message differs.
fn cleanup_partial_upload(
    session: &CartSession,
    cart_path: &str,
    existed_before: bool,
    wrote_any: bool,
    e: io::Error,
) -> io::Error {
    if !wrote_any {
        // The failure came before any byte reached the card, so there is no partial file of ours.
        return e;
    }
    if existed_before {
        // We were overwriting: the original is already gone and this partial is all that is left,
        // so removing it would destroy the only copy on the card. Say so instead.
        return err(format!(
            "{e} — {cart_path} was being overwritten and is now incomplete; upload it again to restore it"
        ));
    }
    match session.remove_cart_path(cart_path) {
        Ok(()) => err(format!(
            "{e} — removed the incomplete {cart_path} from the card"
        )),
        Err(rm) => err(format!(
            "{e} — could not remove the incomplete {cart_path} from the card ({rm}); delete it manually before using it"
        )),
    }
}

/// `--upload`: copy one host file onto the card and exit, without running the suite.
///
/// Writes exactly this one file. `skip_existing` is `false`, so a same-named file is replaced —
/// what it replaced is printed first, because this can be pointed at a card holding real data.
fn upload_only(
    session: &CartSession,
    src: &Path,
    parent: &str,
    dest_name: Option<&str>,
    port: &str,
) -> io::Result<()> {
    let meta = std::fs::metadata(src)?;
    if !meta.is_file() {
        // `import_from_pc_with_progress` also takes directories, but the overwrite report and the
        // byte accounting below only describe a single file, so do not pretend to handle a tree.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a file", src.display()),
        ));
    }
    let total = meta.len();
    let name = match dest_name {
        Some(n) => n.to_string(),
        None => src
            .file_name()
            .ok_or_else(|| err(format!("{} has no file name", src.display())))?
            .to_string_lossy()
            .into_owned(),
    };
    let cart_path = cart_join(parent, &name);

    println!("uploading {} ({total} bytes)", src.display());
    println!("       to {cart_path}");

    // Say what is about to be destroyed before destroying it.
    let kind = session.cart_path_entry_kind(&cart_path)?;
    match kind {
        Some(true) => {
            // The import would fail with AlreadyExists; failing here says why in one line.
            return Err(err(format!("{cart_path} exists and is a directory")));
        }
        Some(false) => {
            let existing = session.total_bytes_for_cart_entry(&cart_path)?;
            println!("  OVERWRITING existing {cart_path} ({existing} bytes)");
        }
        None => println!("  no existing {cart_path}; creating"),
    }
    // Decides whether a partial file left by a failed write is ours to delete, or the remains of
    // something that was already on the card.
    let existed_before = kind == Some(false);

    // `progress` reports a delta per chunk, not a running total (partition.rs:291); returning false
    // would abort the transfer.
    let t = Instant::now();
    let mut done = 0u64;
    let mut last_print = 0u64;
    let mut wrote_any = false;
    let written = session.import_from_pc_with_progress(src, parent, &name, false, |n| {
        done += n;
        wrote_any = true;
        if done - last_print >= 64 * 1024 || done == total {
            last_print = done;
            let pct = (done * 100).checked_div(total).unwrap_or(100);
            print!("\r  {done}/{total} bytes ({pct}%)");
            let _ = io::stdout().flush();
        }
        true
    });
    if last_print > 0 {
        println!();
    }
    if let Err(e) = written {
        return Err(cleanup_partial_upload(
            session,
            &cart_path,
            existed_before,
            wrote_any,
            e,
        ));
    }

    let ms = t.elapsed().as_millis();
    if done != total {
        // The write reported success, so the file on the card is most likely complete and the
        // deltas under-reported it (the suite checks exactly this accounting). Deleting here could
        // destroy a good upload, so report the discrepancy and leave the file alone.
        return Err(err(format!(
            "progress deltas sum to {done} of {total} bytes; {cart_path} was written but may be incomplete — verify it before trusting it"
        )));
    }
    println!("wrote {done} bytes in {ms}ms");

    // A successful write only means the write path returned Ok; `--verify` reads the bytes back.
    println!(
        "verify with: sc64-sd-e2e --port {port} --verify {cart_path} --against {}",
        src.display()
    );
    Ok(())
}

/// `--verify`: read a cart file back and compare it to a host file. Read-only.
///
/// An upload reporting success only means the write path returned `Ok`; this reads the bytes back
/// through the cart's own filesystem and compares them, which is what "the file is really there"
/// actually requires.
///
/// Returns the exit code instead of calling [`std::process::exit`], so `main` can release the
/// cart's SD session before the process ends — see [`run`].
fn verify_only(session: &CartSession, cart_path: &str, host: &Path) -> io::Result<i32> {
    let expect = std::fs::read(host)?;
    println!("verifying {cart_path}");
    println!("  against {} ({} bytes)", host.display(), expect.len());

    match session.cart_path_entry_kind(cart_path)? {
        None => {
            eprintln!("  FAIL: not present on the cart");
            return Ok(1);
        }
        Some(true) => {
            eprintln!("  FAIL: cart path is a directory");
            return Ok(1);
        }
        Some(false) => {}
    }

    let listed = session.total_bytes_for_cart_entry(cart_path)?;
    println!("  cart reports {listed} bytes");

    let dest = std::env::temp_dir().join("sc64-sd-e2e-verify.bin");
    let _ = std::fs::remove_file(&dest);
    session.copy_cart_entry_to_host_with_progress(cart_path, &dest, false, |_| true)?;
    let got = std::fs::read(&dest)?;
    let _ = std::fs::remove_file(&dest);

    if got.len() != expect.len() {
        eprintln!(
            "  FAIL: read back {} bytes, expected {}",
            got.len(),
            expect.len()
        );
        return Ok(1);
    }
    if let Some(i) = got.iter().zip(&expect).position(|(a, b)| a != b) {
        eprintln!("  FAIL: first byte difference at offset {i}");
        return Ok(1);
    }
    println!("  OK: {} bytes identical", expect.len());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::cart_join;

    /// The path printed for the overwrite report and the `--verify` hint has to be the same path
    /// `import_from_pc_with_progress` writes to, whatever shape `--to` was given in.
    #[test]
    fn cart_join_matches_cart_parent_trimmed() {
        for parent in ["/", "", "  ", "///"] {
            assert_eq!(
                cart_join(parent, "rom.z64"),
                "/rom.z64",
                "parent {parent:?}"
            );
        }
        for parent in ["/roms", "roms", "/roms/", "roms/", " /roms/ ", "\\roms"] {
            assert_eq!(
                cart_join(parent, "rom.z64"),
                "/roms/rom.z64",
                "parent {parent:?}"
            );
        }
        assert_eq!(cart_join("/a/b", "c.bin"), "/a/b/c.bin");
    }
}
