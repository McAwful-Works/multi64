//! Hardware check for #200: a cancelled replace leaves the original on the card.
//!
//! Goes through `CartSession::import_from_pc_with_progress`, the call Xfer64 makes, because part of
//! #200 was in the session itself: on cancel it used to remove the destination, which after the
//! reordering would be the untouched original. Only a real session exercises that.
//!
//! Imports a file, then imports a larger replacement and cancels it after the first chunk, then
//! imports the replacement to completion; the card is read back byte for byte after each step. The
//! file is removed at the end.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example replace_keeps_original -- --port COM4 --scratch <dir>
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    port: String,
    #[arg(long, default_value_t = 115200)]
    baud: u32,
    /// Host scratch directory for source and read-back files.
    #[arg(long)]
    scratch: PathBuf,
}

const NAME: &str = "t200_replace.bin";

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| seed ^ (i % 251) as u8 ^ (i >> 10) as u8)
        .collect()
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.scratch)?;
    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);
    let _ = session.remove_cart_path(NAME);

    let mut fails = 0;
    let out = run(&session, &args.scratch, &mut fails);

    match session.remove_cart_path(NAME) {
        Ok(()) => println!("\n  removed /{NAME}"),
        Err(e) => println!("\n  WARNING  could not remove /{NAME}: {e}"),
    }
    match (&out, fails) {
        (Ok(()), 0) => println!("=== ALL CHECKS PASSED"),
        (Ok(()), n) => println!("=== {n} CHECK(S) FAILED"),
        (Err(e), _) => println!("=== ABORTED: {e}"),
    }
    out
}

fn check(fails: &mut u32, ok: bool, what: &str) {
    println!("  {}  {what}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        *fails += 1;
    }
}

fn source(scratch: &Path, tag: &str, body: &[u8]) -> io::Result<PathBuf> {
    let p = scratch.join(format!("src_{tag}.bin"));
    std::fs::write(&p, body)?;
    Ok(p)
}

fn on_card(session: &CartSession, scratch: &Path) -> io::Result<(Vec<u8>, usize, u64)> {
    let dest = scratch.join("back.bin");
    let _ = std::fs::remove_file(&dest);
    session.copy_cart_entry_to_host_with_progress(NAME, &dest, false, |_| true)?;
    let listed: Vec<_> = session
        .list_dir("/")?
        .into_iter()
        .filter(|e| e.name.starts_with("t200_"))
        .collect();
    let size = listed.first().map(|e| e.size).unwrap_or(0);
    Ok((std::fs::read(&dest)?, listed.len(), size))
}

fn run(session: &CartSession, scratch: &Path, fails: &mut u32) -> io::Result<()> {
    let original = pattern(300 * 1024 + 17, 0x3C);
    let replacement = pattern(600 * 1024 + 5, 0xA7);

    println!("=== 1. import the original");
    let src = source(scratch, "original", &original)?;
    session.import_from_pc_with_progress(&src, "/", NAME, false, |_| true)?;
    let (back, n, _) = on_card(session, scratch)?;
    check(
        fails,
        back == original && n == 1,
        "the original is on the card",
    );

    println!("\n=== 2. replace it, cancelling after the first chunk");
    let src = source(scratch, "replacement", &replacement)?;
    let mut chunks = 0;
    let err = session
        .import_from_pc_with_progress(&src, "/", NAME, false, |_| {
            chunks += 1;
            chunks < 2
        })
        .expect_err("the replace was cancelled");
    println!("  import returned: {err} ({:?})", err.kind());
    check(
        fails,
        err.kind() == io::ErrorKind::Interrupted,
        "the import reports it was cancelled",
    );
    let (back, n, size) = on_card(session, scratch)?;
    check(
        fails,
        back == original,
        "the original is byte-for-byte intact",
    );
    check(
        fails,
        n == 1 && size == original.len() as u64,
        "listed once, at the original's size",
    );

    println!("\n=== 3. replace it for real");
    session.import_from_pc_with_progress(&src, "/", NAME, false, |_| true)?;
    let (back, n, size) = on_card(session, scratch)?;
    check(fails, back == replacement, "the replacement is on the card");
    check(
        fails,
        n == 1 && size == replacement.len() as u64,
        "listed once, at the replacement's size",
    );
    Ok(())
}
