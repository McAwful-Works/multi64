//! Re-measure #188 on hardware after the #195 mkdir rewrite.
//!
//! #188 measured exFAT directory creates getting linearly slower as a folder fills: 72 creates took
//! 891s, the last of them ~21s each. That was measured against the old `mkdir_cart_exfat`, which
//! navigated with hadris's `open_dir` and let hadris's `find_free_entry_slots` pick the slots —
//! three scans per create, each reading a whole 512-byte USB sector per 32-byte slot.
//!
//! #195 replaced that path: `mkdir_cart_exfat` now resolves through this crate's own reader and
//! places the entry set itself, so a create is two scans, both through `ExfatSlotReader`, which
//! reads 256 slots (8 KiB) per USB round trip. That predicts roughly 16x fewer reads and one fewer
//! scan — but nothing has re-measured it, and #188's numbers are still the ones on the issue.
//!
//! **Phase A reproduces #188's shape exactly** so the numbers are comparable: 72 directories with
//! 180-character names (14 entry slots each, 72 * 14 = 1008 of a 32 KiB cluster's 1024 slots) in a
//! directory that starts empty. #188's blank card grew its root the same way.
//!
//! **Phase B is the case #188 could not reach**: creates straight into a root that already spans 18
//! clusters. The old code never got here — #177's guard refused a chained root one step earlier —
//! so this has no "before" to compare against. It is the worst case a real user meets.
//!
//! Everything it creates is named with the `t188_` prefix and removed again, including on the error
//! paths. It writes nothing else.
//!
//! ## What it found, on an SC64 with a 29.72 GB exFAT card
//!
//! | Created so far | #188 | after #195 |
//! |---|---|---|
//! | 1-10 | ~2.8s | 5.36s |
//! | 30-40 | ~12s | 5.44s |
//! | 60-72 | ~21s | 5.54s |
//! | slowdown across the run | ~7.5x | **1.03x** |
//!
//! 72 creates in 391.5s against 891s. The linear slowdown is gone, but the cost per create roughly
//! doubled at the start, and phase B's creates into an 18-cluster root came out *faster* (5.13s)
//! than phase A's into a one-cluster directory. A cost that grows with neither entries nor clusters
//! is not the slot scan, so what remains is a fixed per-operation cost — see `exfat_op_cost`.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example exfat_mkdir_timing -- --port COM4
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "exfat-mkdir-timing",
    about = "Time exFAT directory creates, reproducing #188's measurement after the #195 rewrite"
)]
struct Args {
    /// Cart serial device (e.g. COM4).
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    /// Directories to create in phase A. #188 used 72.
    #[arg(long, default_value_t = 72)]
    count: usize,

    /// Name length in characters. #188 used 180, which is 14 entry slots.
    #[arg(long, default_value_t = 180)]
    name_len: usize,

    /// Creates straight into the chained root in phase B.
    #[arg(long, default_value_t = 5)]
    root_samples: usize,

    /// Skip phase B.
    #[arg(long)]
    no_root_phase: bool,
}

/// `t188_<n>_` padded with `x` to `len` characters.
fn probe_name(n: usize, len: usize) -> String {
    let mut s = format!("t188_{n:04}_");
    while s.chars().count() < len {
        s.push('x');
    }
    s
}

/// Entry slots an exFAT entry set of this name length occupies: File + Stream + ceil(len/15) Name.
fn slots_for(len: usize) -> usize {
    2 + len.div_ceil(15)
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Mean of `v[a..b]`, the bands #188's table reports.
fn band(v: &[Duration], a: usize, b: usize) -> Option<f64> {
    let b = b.min(v.len());
    if a >= b {
        return None;
    }
    Some(v[a..b].iter().map(|d| secs(*d)).sum::<f64>() / (b - a) as f64)
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    println!("exFAT mkdir timing - re-measuring #188 after #195");
    println!("  port        {} @ {}", args.port, args.baud);
    println!(
        "  phase A     {} creates, {}-char names = {} entry slots each",
        args.count,
        args.name_len,
        slots_for(args.name_len)
    );
    println!();

    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);

    let scratch = "t188_timing";
    // Anything left by an interrupted earlier run, so a rerun starts from an empty directory.
    let _ = session.remove_cart_path(scratch);

    let out = run(&session, &args, scratch);

    println!("\n=== cleanup");
    match session.remove_cart_path(scratch) {
        Ok(()) => println!("  removed /{scratch}"),
        Err(e) => println!("  WARNING  could not remove /{scratch}: {e}"),
    }
    out
}

fn run(session: &CartSession, args: &Args, scratch: &str) -> io::Result<()> {
    // --- phase A: a directory that starts empty, exactly #188's shape ---------------------------
    println!("=== phase A: creates in a directory that starts empty");
    session.mkdir_cart(scratch)?;

    let mut times: Vec<Duration> = Vec::with_capacity(args.count);
    let phase_a_start = Instant::now();
    for i in 0..args.count {
        let path = format!("{scratch}/{}", probe_name(i, args.name_len));
        let t = Instant::now();
        match session.mkdir_cart(&path) {
            Ok(()) => times.push(t.elapsed()),
            Err(e) => {
                println!("  create {i} failed after {:.2}s: {e}", secs(t.elapsed()));
                println!("  (stopping phase A here; {} completed)", times.len());
                break;
            }
        }
        if i < 3 || (i + 1) % 10 == 0 {
            println!(
                "  {:>3} created  {:.2}s",
                i + 1,
                secs(times[times.len() - 1])
            );
        }
    }
    let phase_a = phase_a_start.elapsed();

    println!("\n  --- per-create seconds, in #188's bands ---");
    println!("  {:<16} {:>10} {:>10}", "created so far", "#188", "now");
    let rows: [(&str, usize, usize, &str); 3] = [
        ("1-10", 0, 10, "~2.8"),
        ("30-40", 29, 40, "~12"),
        ("60-72", 59, 72, "~21"),
    ];
    for (label, a, b, before) in rows {
        match band(&times, a, b) {
            Some(now) => println!("  {label:<16} {before:>10} {now:>10.2}"),
            None => println!("  {label:<16} {before:>10} {:>10}", "n/a"),
        }
    }
    println!(
        "\n  total {} creates: {:.1}s   (#188: 891s for 72)",
        times.len(),
        secs(phase_a)
    );
    if let (Some(first), Some(last)) = (band(&times, 0, 10), band(&times, 59, 72)) {
        println!(
            "  slowdown across the run: {:.2}x   (#188: ~7.5x)",
            last / first
        );
    }
    if !times.is_empty() {
        let total: f64 = times.iter().map(|d| secs(*d)).sum();
        println!(
            "  mean {:.2}s   min {:.2}s   max {:.2}s",
            total / times.len() as f64,
            times.iter().map(|d| secs(*d)).fold(f64::MAX, f64::min),
            times.iter().map(|d| secs(*d)).fold(0.0, f64::max)
        );
    }

    // --- phase B: the chained root, which #188's code could never reach --------------------------
    if !args.no_root_phase {
        println!("\n=== phase B: creates straight into the chained root");
        let listed = session.list_dir("/")?.len();
        println!("  root currently lists {listed} entries");
        let mut root_times: Vec<Duration> = Vec::new();
        let mut made: Vec<String> = Vec::new();
        for i in 0..args.root_samples {
            let name = probe_name(9000 + i, args.name_len);
            let t = Instant::now();
            match session.mkdir_cart(&name) {
                Ok(()) => {
                    root_times.push(t.elapsed());
                    println!("  {:>3}  {:.2}s", i + 1, secs(root_times[i]));
                    made.push(name);
                }
                Err(e) => {
                    println!("  create {i} failed after {:.2}s: {e}", secs(t.elapsed()));
                    break;
                }
            }
        }
        if !root_times.is_empty() {
            let total: f64 = root_times.iter().map(|d| secs(*d)).sum();
            println!(
                "  mean {:.2}s over {} creates into an 18-cluster root",
                total / root_times.len() as f64,
                root_times.len()
            );
        }
        for name in &made {
            if let Err(e) = session.remove_cart_path(name) {
                println!("  WARNING  could not remove /{name}: {e}");
            }
        }
        println!("  removed {} root probe directories", made.len());
    }

    Ok(())
}
