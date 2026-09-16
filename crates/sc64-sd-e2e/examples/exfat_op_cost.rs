//! Break one exFAT `mkdir` into its parts, to find what the flat per-create cost actually is.
//!
//! The #188 re-measurement (`exfat_mkdir_timing`) showed the linear slowdown gone — 1.03x across 72
//! creates where it used to be ~7.5x — but left every create costing about the same 5.4s, and
//! creates into an 18-cluster root came out *slightly faster* than creates into a one-cluster
//! directory. A cost that does not grow with the directory, and does not grow with the number of
//! clusters to walk, is not the slot scan #188 was about. It is a fixed cost paid once per call.
//!
//! This separates the candidates by timing operations that share some of that fixed cost but not
//! all of it:
//!
//! - **`list_dir`** opens the volume and reads a directory, and writes nothing. Whatever it costs is
//!   the read-side floor.
//! - **`list_dir` on an empty directory** subtracts the cost of actually reading 1273 entries.
//! - **`mkdir` then the matching `remove`** adds the write side: a 32 KiB zero-filled cluster, the
//!   FAT entry, the entry set, and `sync_bitmap`.
//!
//! It creates only `t188_cost*` names and removes them again.
//!
//! ## What it found, on an SC64 with a 29.72 GB exFAT card
//!
//! ```text
//! list_dir / (1273 entries, 18 clusters)   mean 2.09s
//! list_dir an empty directory              mean 2.11s
//! mkdir in that empty directory            mean 5.24s
//! remove each of those again               mean 8.30s
//! ```
//!
//! Reading 1273 entries across 18 clusters costs **-0.02s more than reading none**: at this card's
//! scale the directory read is free, and the whole read cost is fixed overhead paid before any
//! directory data is touched. One operation is roughly 2.1s fixed, plus 3.1s of writes for a mkdir
//! and 6.2s for a remove — which makes `remove` the most expensive operation here and the one
//! nothing has looked at.
//!
//! `sd_read_throughput` takes the next step and shows the bitmap load explains only 0.37s of that
//! 2.1s.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example exfat_op_cost -- --port COM4
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64SdSession};
use std::io;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "exfat-op-cost", about = "Time the parts of one exFAT mkdir")]
struct Args {
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    /// Repeats per operation.
    #[arg(long, default_value_t = 3)]
    reps: usize,
}

/// Run `op` `reps` times, print each time and the mean.
fn timed<F>(label: &str, reps: usize, mut op: F) -> io::Result<f64>
where
    F: FnMut(usize) -> io::Result<()>,
{
    let mut all = Vec::with_capacity(reps);
    for i in 0..reps {
        let t = Instant::now();
        op(i)?;
        all.push(t.elapsed().as_secs_f64());
    }
    let mean = all.iter().sum::<f64>() / all.len() as f64;
    let each: Vec<String> = all.iter().map(|s| format!("{s:.2}")).collect();
    println!("  {label:<38} mean {mean:>6.2}s   [{}]", each.join(", "));
    Ok(mean)
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    println!("exFAT operation cost breakdown");
    println!("  port  {} @ {}\n", args.port, args.baud);

    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);
    let scratch = "t188_cost";
    let _ = session.remove_cart_path(scratch);

    let out = run(&session, &args, scratch);

    match session.remove_cart_path(scratch) {
        Ok(()) => println!("\n  cleaned up /{scratch}"),
        Err(e) => println!("\n  WARNING  could not remove /{scratch}: {e}"),
    }
    out
}

fn run(session: &CartSession, args: &Args, scratch: &str) -> io::Result<()> {
    let reps = args.reps;

    // Read-only, on the big chained root: the floor for opening the volume and reading a directory.
    let root_list = timed("list_dir / (1273 entries, 18 clusters)", reps, |_| {
        session.list_dir("/").map(|_| ())
    })?;

    // The scratch directory itself is the first write, and is not timed as part of the comparison.
    session.mkdir_cart(scratch)?;

    let empty_list = timed("list_dir an empty directory", reps, |_| {
        session.list_dir(scratch).map(|_| ())
    })?;

    let mkdir = timed("mkdir in that empty directory", reps, |i| {
        session.mkdir_cart(&format!("{scratch}/d{i}"))
    })?;

    let rmdir = timed("remove each of those again", reps, |i| {
        session.remove_cart_path(&format!("{scratch}/d{i}"))
    })?;

    println!("\n=== what the numbers separate");
    println!(
        "  reading 1273 entries costs {:.2}s more than reading none",
        root_list - empty_list
    );
    println!(
        "  the write side of a mkdir adds {:.2}s over just reading the directory",
        mkdir - empty_list
    );
    println!("  a remove costs {rmdir:.2}s, against a mkdir's {mkdir:.2}s");
    println!(
        "\n  If `list_dir` on an EMPTY directory already costs most of a mkdir, the fixed cost is\n  \
         in opening the volume, not in the create. If it is near zero, the cost is the create's\n  \
         own writes: the 32 KiB zero-filled cluster, the FAT entry, and `sync_bitmap`."
    );
    Ok(())
}
