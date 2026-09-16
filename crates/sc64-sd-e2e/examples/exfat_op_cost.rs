//! Break one exFAT `mkdir` into its parts, to find what the flat per-create cost actually is.
//!
//! The #188 re-measurement (`exfat_mkdir_timing`) showed the linear slowdown gone — 1.03x across 72
//! creates where it used to be ~7.5x — but left every create costing about the same 5.4s, and
//! creates into an 18-cluster root came out *slightly faster* than creates into a one-cluster
//! directory. A cost that does not grow with the directory, and does not grow with the number of
//! clusters to walk, is not the slot scan #188 was about. It is a fixed cost paid once per call —
//! or so it looked; the findings below say why that framing was wrong.
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
//! It creates only `t188_cost*` and `t188_cmd*` names and removes them again.
//!
//! ## What it found, on an SC64 with a 29.72 GB exFAT card
//!
//! Before the session read cache:
//!
//! ```text
//! list_dir / (1273 entries, 18 clusters)   mean 2.09s
//! list_dir an empty directory              mean 2.11s
//! mkdir in that empty directory            mean 5.24s
//! remove each of those again               mean 8.30s
//! ```
//!
//! **The conclusion first drawn from this was wrong.** The two lists matched not because reading
//! entries is free, but because the "empty directory" is a folder *in* the root, so reaching it read
//! the whole 576 KiB root first: both lines measure the same root read. See the correction on #196
//! and `exfat_root_read_cost`, which times that read on its own. mkdir and remove were slow because
//! each resolves the root several times per command.
//!
//! With the session read cache (#196), on the same card, against the same build with cache lookups
//! disabled so both columns are measured the same way:
//!
//! | | no cache | cache |
//! |---|---|---|
//! | list the root, again in the same session | 2.07s | 0.00s |
//! | mkdir, same session | 5.20s | 0.46s |
//! | remove, same session | 8.27s | 0.48s |
//! | **mkdir, one session per command** (Xfer64) | **4.43s** | **3.30s** |
//! | **remove, one session per command** (Xfer64) | **7.55s** | **3.32s** |
//!
//! Per click the cache removes the *repeat* reads of the root; the first read in each session still
//! goes to the card, and at ~320 KiB/s a 576 KiB root is most of what is left.
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
    drop(session);
    out?;
    per_command_sessions(&args)
}

/// The same mkdir and remove, each in a session of its own — which is how Xfer64 runs every command
/// (`with_session`). The session read cache (#196) does not outlive a session, so this is the cost a
/// user sees per click: one read of each directory on the path, plus the command's own writes.
fn per_command_sessions(args: &Args) -> io::Result<()> {
    println!("\n=== one session per command, as Xfer64 runs them");
    let open = || -> io::Result<CartSession> {
        Ok(CartSession::Sc64(Sc64SdSession::open(
            &args.port, args.baud,
        )?))
    };
    let name = |i: usize| format!("t188_cmd{i}");
    let mkdir = timed(
        "mkdir in the root, own session",
        args.reps,
        |i| -> io::Result<()> { open()?.mkdir_cart(&name(i)) },
    )?;
    let remove = timed("remove it, own session", args.reps, |i| -> io::Result<()> {
        open()?.remove_cart_path(&name(i))
    })?;
    println!(
        "  per click: mkdir {mkdir:.2}s, remove {remove:.2}s (session open and close included)"
    );
    Ok(())
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

    println!("\n  all in one session: list {root_list:.2}s, empty-folder list {empty_list:.2}s, mkdir {mkdir:.2}s, remove {rmdir:.2}s");
    println!(
        "  (the 'empty' folder is reached through the root, so both lists read the same root; within\n  \
         one session the read cache serves every read of it after the first)"
    );
    Ok(())
}
