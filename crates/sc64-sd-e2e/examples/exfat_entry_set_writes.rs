//! Hardware check for #215 and #220: exFAT entry sets written a sector at a time, and a source that
//! grows during an import.
//!
//! Every exFAT entry write now goes out as whole sectors, so this runs each operation that writes
//! an entry set through `CartSession`, the call Xfer64 makes, and reads the card back after each:
//! a nested mkdir, an import under a 240-character name (an 18-slot set, which always crosses a
//! sector), a replace of it, renames long to short and back, an import into the root, and an
//! import whose source grows part-way (which must be refused and leave nothing). Everything it
//! makes is under `/t215` or named `t215_*` in the root, and is removed at the end.
//!
//! Run on an SC64 with the 29.72 GB exFAT test card (32 KiB clusters, an 18-cluster chained root)
//! before #229 merged: every check passed, and `exfat_raw_root_walk` then decoded the same 1273
//! root entries the cart listed, none of them left by this run.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example exfat_entry_set_writes -- --port COM4 --scratch <dir>
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io::{self, Write};
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

const DIR: &str = "/t215/sub/deeper";
const ROOT_FILE: &str = "t215_root.bin";

fn long_name(tag: char) -> String {
    format!("t215_{}.bin", tag.to_string().repeat(231))
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| seed ^ (i % 251) as u8 ^ (i >> 10) as u8)
        .collect()
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

/// The file's bytes read back, and the sizes of every listed entry with that name.
fn on_card(
    session: &CartSession,
    scratch: &Path,
    parent: &str,
    name: &str,
) -> io::Result<(Option<Vec<u8>>, Vec<u64>)> {
    let listed: Vec<u64> = session
        .list_dir(parent)?
        .into_iter()
        .filter(|e| e.name == name)
        .map(|e| e.size)
        .collect();
    if listed.is_empty() {
        return Ok((None, listed));
    }
    let dest = scratch.join("back.bin");
    let _ = std::fs::remove_file(&dest);
    let path = format!("{}/{name}", parent.trim_end_matches('/'));
    session.copy_cart_entry_to_host_with_progress(&path, &dest, false, |_| true)?;
    Ok((Some(std::fs::read(&dest)?), listed))
}

fn expect_file(
    fails: &mut u32,
    session: &CartSession,
    scratch: &Path,
    parent: &str,
    name: &str,
    body: &[u8],
    what: &str,
) -> io::Result<()> {
    let (back, listed) = on_card(session, scratch, parent, name)?;
    check(
        fails,
        back.as_deref() == Some(body) && listed == [body.len() as u64],
        what,
    );
    Ok(())
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.scratch)?;
    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);
    println!("exFAT: {}", session.is_exfat());

    let mut fails = 0;
    let out = run(&session, &args.scratch, &mut fails);

    println!("\n=== cleanup");
    let mut left = 0;
    for path in [
        format!("{DIR}/{}", long_name('a')),
        format!("{DIR}/{}", long_name('b')),
        format!("{DIR}/t215_short.bin"),
        format!("{DIR}/t215_grow.bin"),
        DIR.to_string(),
        "/t215/sub".to_string(),
        "/t215".to_string(),
        format!("/{ROOT_FILE}"),
    ] {
        match session.cart_path_entry_kind(&path) {
            Ok(None) => {}
            _ => match session.remove_cart_path(&path) {
                Ok(()) => println!("  removed {}", short(&path)),
                Err(e) => {
                    left += 1;
                    println!("  WARNING  could not remove {}: {e}", short(&path));
                }
            },
        }
    }
    let root = session.list_dir("/")?;
    let strays = root.iter().filter(|e| e.name.starts_with("t215")).count();
    check(
        &mut fails,
        strays == 0 && left == 0,
        "nothing named t215 left in the root",
    );
    println!("  root lists {} entries", root.len());

    match (&out, fails) {
        (Ok(()), 0) => println!("=== ALL CHECKS PASSED"),
        (Ok(()), n) => println!("=== {n} CHECK(S) FAILED"),
        (Err(e), _) => println!("=== ABORTED: {e}"),
    }
    out
}

fn short(path: &str) -> String {
    if path.len() > 60 {
        format!("{}…", &path[..60])
    } else {
        path.to_string()
    }
}

fn run(session: &CartSession, scratch: &Path, fails: &mut u32) -> io::Result<()> {
    println!("\n=== 1. nested mkdir {DIR}");
    session.mkdir_cart(DIR)?;
    check(
        fails,
        session.cart_path_entry_kind(DIR)? == Some(true),
        "the nested folder exists",
    );

    let original = pattern(100 * 1024 + 3, 0x3C);
    let replacement = pattern(40 * 1024 + 9, 0xA7);
    let a = long_name('a');
    let b = long_name('b');

    println!("\n=== 2. import under a 240-character name");
    let src = source(scratch, "original", &original)?;
    session.import_from_pc_with_progress(&src, DIR, &a, false, |_| true)?;
    expect_file(
        fails,
        session,
        scratch,
        DIR,
        &a,
        &original,
        "read back intact, listed once",
    )?;

    println!("\n=== 3. replace it");
    let src = source(scratch, "replacement", &replacement)?;
    session.import_from_pc_with_progress(&src, DIR, &a, false, |_| true)?;
    expect_file(
        fails,
        session,
        scratch,
        DIR,
        &a,
        &replacement,
        "the replacement, listed once at its size",
    )?;

    println!("\n=== 4. rename long to short, then short to another long name");
    session.rename_cart(&format!("{DIR}/{a}"), &format!("{DIR}/t215_short.bin"))?;
    expect_file(
        fails,
        session,
        scratch,
        DIR,
        "t215_short.bin",
        &replacement,
        "renamed to the short name",
    )?;
    check(
        fails,
        on_card(session, scratch, DIR, &a)?.1.is_empty(),
        "the long name is gone",
    );
    session.rename_cart(&format!("{DIR}/t215_short.bin"), &format!("{DIR}/{b}"))?;
    expect_file(
        fails,
        session,
        scratch,
        DIR,
        &b,
        &replacement,
        "renamed to the other long name",
    )?;

    println!("\n=== 5. import into the root");
    let root_body = pattern(70 * 1024 + 1, 0x51);
    let src = source(scratch, "root", &root_body)?;
    session.import_from_pc_with_progress(&src, "/", ROOT_FILE, false, |_| true)?;
    expect_file(
        fails,
        session,
        scratch,
        "/",
        ROOT_FILE,
        &root_body,
        "read back from the root, listed once",
    )?;

    println!("\n=== 6. import a file that grows while it is copied");
    let src = source(scratch, "grow", &pattern(100 * 1024, 0x77))?;
    let mut grown = false;
    let err = session
        .import_from_pc_with_progress(&src, DIR, "t215_grow.bin", false, |_| {
            if !grown {
                grown = true;
                if let Err(e) = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&src)
                    .and_then(|mut f| f.write_all(&[0xEEu8; 64 * 1024]))
                {
                    println!("  could not grow the source: {e}");
                }
            }
            true
        })
        .err();
    match &err {
        Some(e) => println!("  import returned: {e} ({:?})", e.kind()),
        None => println!("  import returned Ok"),
    }
    check(
        fails,
        err.as_ref().is_some_and(|e| e.to_string().contains("grew")),
        "the import is refused because the source grew",
    );
    check(
        fails,
        on_card(session, scratch, DIR, "t215_grow.bin")?
            .1
            .is_empty(),
        "nothing is left under its name",
    );
    Ok(())
}
