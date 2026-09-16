//! Hardware check for #190's import path on a card whose root spans several clusters.
//!
//! Imports through `CartSession::import_from_pc_with_progress` — what Xfer64 calls — then reads
//! every file back byte for byte and checks the root listing. Leaves the files on the card so a
//! `chkdsk` in a PC reader can check the allocation afterwards; `--cleanup` removes them.
//!
//! ## What it found, on an SC64 with a 29.72 GB exFAT card
//!
//! The card's root was 18 non-contiguous clusters holding 1272 entries, which #177 refused to add
//! to at all. Every check passed: a 4 KiB file, a ten-cluster 300 KiB file, a new folder with a
//! file inside it, and a replace, each read back byte for byte; the root went from 1272 entries to
//! exactly 1275. In a PC reader afterwards, Windows read all three files identical to their
//! sources, and `chkdsk` found no problems with exactly the 15 clusters those files and the folder
//! need newly in use — so the replace also released its original cluster.
//!
//! Not reached: the fragmented allocation path, since the card's free space was contiguous.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example exfat_chained_root_import -- --port COM4
//! cargo run -p sc64-sd-e2e --release --example exfat_chained_root_import -- --port COM4 --cleanup
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    port: String,
    #[arg(long, default_value_t = 115200)]
    baud: u32,
    /// Host scratch directory for source and read-back files.
    #[arg(long)]
    scratch: PathBuf,
    /// Remove this probe's files from the card and exit.
    #[arg(long)]
    cleanup: bool,
}

const SMALL: &str = "t190_small.bin";
const MULTI: &str = "t190_multi.bin";
const DIR: &str = "t190_dir";
const NESTED: &str = "nested.bin";

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| seed ^ (i % 251) as u8 ^ (i >> 9) as u8)
        .collect()
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.scratch)?;
    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);

    if args.cleanup {
        for p in [SMALL, MULTI, DIR] {
            match session.remove_cart_path(p) {
                Ok(()) => println!("removed /{p}"),
                Err(e) => println!("/{p}: {e}"),
            }
        }
        let n = session.list_dir("/")?.len();
        println!("root now lists {n} entries");
        return Ok(());
    }

    let mut fails = 0;
    let out = run(&session, &args.scratch, &mut fails);
    match out {
        Ok(()) if fails == 0 => println!("\n=== ALL CHECKS PASSED"),
        Ok(()) => println!("\n=== {fails} CHECK(S) FAILED"),
        Err(ref e) => println!("\n=== ABORTED: {e}"),
    }
    out
}

fn check(fails: &mut u32, ok: bool, what: &str) {
    println!("  {}  {what}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        *fails += 1;
    }
}

fn import(
    session: &CartSession,
    scratch: &Path,
    parent: &str,
    name: &str,
    body: &[u8],
) -> io::Result<()> {
    let src = scratch.join(format!("src_{name}"));
    std::fs::write(&src, body)?;
    let t = Instant::now();
    session.import_from_pc_with_progress(&src, parent, name, false, |_| true)?;
    println!(
        "  imported {}/{name}: {} bytes in {:.2}s",
        parent.trim_end_matches('/'),
        body.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

fn read_back(session: &CartSession, scratch: &Path, cart: &str) -> io::Result<Vec<u8>> {
    let dest = scratch.join(format!("back_{}", cart.replace('/', "_")));
    let _ = std::fs::remove_file(&dest);
    session.copy_cart_entry_to_host_with_progress(cart, &dest, false, |_| true)?;
    std::fs::read(&dest)
}

fn run(session: &CartSession, scratch: &Path, fails: &mut u32) -> io::Result<()> {
    println!("=== before");
    for p in [SMALL, MULTI, DIR] {
        let _ = session.remove_cart_path(p);
    }
    let before = session.list_dir("/")?;
    println!("  root lists {} entries", before.len());

    println!("\n=== 1. small file into the chained root");
    let small = pattern(4096, 0x11);
    import(session, scratch, "/", SMALL, &small)?;

    println!("\n=== 2. multi-cluster file (not a whole number of 32 KiB clusters)");
    let multi = pattern(300 * 1024 + 123, 0x5A);
    import(session, scratch, "/", MULTI, &multi)?;

    println!("\n=== 3. folder in the chained root, then a file inside it");
    session.mkdir_cart(DIR)?;
    let nested = pattern(70 * 1024, 0x77);
    import(session, scratch, DIR, NESTED, &nested)?;

    println!("\n=== 4. replace the small file with different contents");
    let replacement = pattern(9000, 0xC3);
    import(session, scratch, "/", SMALL, &replacement)?;

    println!("\n=== read back, byte for byte");
    check(
        fails,
        read_back(session, scratch, SMALL)? == replacement,
        "t190_small.bin holds the replacement",
    );
    check(
        fails,
        read_back(session, scratch, MULTI)? == multi,
        "t190_multi.bin matches",
    );
    check(
        fails,
        read_back(session, scratch, &format!("{DIR}/{NESTED}"))? == nested,
        "t190_dir/nested.bin matches",
    );

    println!("\n=== listings");
    let after = session.list_dir("/")?;
    println!("  root lists {} entries", after.len());
    check(
        fails,
        after.len() == before.len() + 3,
        "exactly three entries added to the root",
    );
    for (name, size) in [(SMALL, replacement.len()), (MULTI, multi.len())] {
        let hits: Vec<_> = after.iter().filter(|e| e.name == name).collect();
        check(
            fails,
            hits.len() == 1 && hits[0].size == size as u64,
            &format!("{name} listed once with size {size}"),
        );
    }
    check(
        fails,
        after.iter().filter(|e| e.name == DIR && e.is_dir).count() == 1,
        "t190_dir listed once as a folder",
    );
    let inner = session.list_dir(DIR)?;
    check(
        fails,
        inner.len() == 1 && inner[0].name == NESTED && inner[0].size == nested.len() as u64,
        "t190_dir holds only nested.bin, with its size",
    );

    println!("\n  Files left on the card for chkdsk. Remove later with --cleanup.");
    Ok(())
}
