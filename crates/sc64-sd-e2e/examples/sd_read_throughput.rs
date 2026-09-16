//! Measure the SD read link's bulk throughput and its per-request latency. **Read-only.**
//!
//! The #188 re-measurement left one number unexplained: every exFAT operation costs about 2.1s
//! before it does anything, whether the directory holds 1273 entries or none. Reading the hadris
//! source explains *what* is being transferred — `ExFatFs::open` calls `bitmap.load`, a single
//! `read_exact` of the whole allocation bitmap, which on a 29.72 GB card with 32 KiB clusters is
//! 973760 bits = 121720 bytes — but not whether that transfer is slow because the link is slow or
//! because it is being issued badly.
//!
//! `SectorPartitionDisk::read` batches up to `SD_CARD_BUFFER_MAX_BYTES` (128 KiB) per
//! `read_sd_sectors`, and hadris's `SectorCursor::read_exact` passes straight through to it, so the
//! bitmap ought to cross in one or two bulk requests. This checks that against the link directly:
//!
//! - **one bitmap-sized read** (121344 bytes, the bitmap rounded down to whole sectors) in a single
//!   request;
//! - **64 separate 512-byte requests**, which is what an unbatched reader would do.
//!
//! If the bulk figure is near 2.1s, the fixed cost is the link and batching has already won
//! whatever there was to win. If it is far below, something is defeating the batch.
//!
//! ## What it found, on an SC64 with a 29.72 GB exFAT card
//!
//! ```text
//! one 121344-byte request (bitmap-sized)   0.37s   324.3 KiB/s
//! 64 separate 512-byte requests            0.35s    92.2 KiB/s
//! per-request latency                      5.4 ms
//! ```
//!
//! So the bitmap crosses in one bulk request and costs **0.37s of the 2.11s**. Batching is working;
//! a sector cache would not help this transfer. One further contributor is visible in the hadris
//! source rather than the timings: `ExFatFs::open` scans the root for system entries with
//! `MAX_SYSTEM_ENTRIES = 100`, reading a 32-byte entry per iteration, and a 32-byte read through
//! `SectorPartitionDisk::read` is a whole 512-byte sector — up to 100 round trips, ~0.54s, exited
//! early only by an `END_OF_DIRECTORY` that a populated root never reaches. That leaves roughly
//! 1.2s per operation unattributed.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example sd_read_throughput -- --port COM4
//! ```

use clap::Parser;
use multi64_sc64_sd::Sc64Link;
use std::io;
use std::time::Instant;

/// The allocation bitmap of the card under test: 973760 clusters, one bit each.
const BITMAP_BYTES: usize = 121_720;

#[derive(Parser, Debug)]
#[command(
    name = "sd-read-throughput",
    about = "Time bulk and per-sector SD reads (read-only)"
)]
struct Args {
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    /// Where to read from. Anywhere is fine; nothing is written.
    #[arg(long, default_value_t = 2048)]
    lba: u64,

    /// Sectors to read one at a time, for the latency figure.
    #[arg(long, default_value_t = 64)]
    single_sectors: usize,
}

fn rate(bytes: usize, secs: f64) -> String {
    format!("{:.1} KiB/s", (bytes as f64 / 1024.0) / secs)
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    println!("SD read throughput (read-only)");
    println!("  port  {} @ {}\n", args.port, args.baud);

    let mut link = Sc64Link::open(&args.port, args.baud)
        .map_err(|e| io::Error::other(format!("serial open: {e}")))?;
    link.identify()?;
    link.sd_init()?;

    let out = measure(&mut link, &args);
    link.sd_deinit_try();
    out
}

fn measure(link: &mut Sc64Link, args: &Args) -> io::Result<()> {
    // Round down to whole sectors: the transport requires a multiple of 512.
    let bulk_bytes = (BITMAP_BYTES / 512) * 512;
    let mut buf = vec![0u8; bulk_bytes];

    // Once to warm whatever caching the cart does, then the timed run.
    link.read_sd_sectors(args.lba, &mut buf)?;
    let t = Instant::now();
    link.read_sd_sectors(args.lba, &mut buf)?;
    let bulk = t.elapsed().as_secs_f64();
    println!(
        "  one {bulk_bytes}-byte request (bitmap-sized)   {bulk:.2}s   {}",
        rate(bulk_bytes, bulk)
    );

    let mut sector = [0u8; 512];
    let t = Instant::now();
    for i in 0..args.single_sectors {
        link.read_sd_sectors(args.lba + i as u64, &mut sector)?;
    }
    let singles = t.elapsed().as_secs_f64();
    let per = singles / args.single_sectors as f64;
    println!(
        "  {} separate 512-byte requests          {singles:.2}s   {}",
        args.single_sectors,
        rate(args.single_sectors * 512, singles)
    );
    println!(
        "  per-request latency                     {:.1} ms",
        per * 1000.0
    );

    println!("\n=== what this says about the 2.1s fixed cost");
    println!(
        "  A bitmap-sized bulk read costs {bulk:.2}s. Every exFAT operation pays one, because\n  \
         `ExFatFs::open` loads the whole allocation bitmap before doing anything."
    );
    let unbatched = per * (bulk_bytes as f64 / 512.0);
    println!(
        "  The same bytes unbatched would cost {unbatched:.1}s, so batching is working: it is\n  \
         already saving {:.0}x on this transfer.",
        unbatched / bulk
    );
    Ok(())
}
