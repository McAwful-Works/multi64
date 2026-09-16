//! What reading a large exFAT root actually costs over the SC64 link, and how much of an
//! operation's fixed cost it is. **Read-only.**
//!
//! #196 attributed a `list_dir`'s ~2.1s to `ExFatFs::open` — the bitmap load and the system-entry
//! scan. But `list_dir_exfat` has not called `ExFatFs::open` since #189. What it does do, for every
//! path including a subfolder's, is resolve through the root: walk the root's FAT chain and read
//! its slots up to the end-of-directory marker, 8 KiB (256 slots) per request. On the test card
//! that marker is in cluster 18 of 18, so every operation reads ~576 KiB of root before anything
//! else. That would also explain why listing an *empty subfolder* cost the same as listing the root.
//!
//! This times that transfer directly, with the requests shaped three ways, against the session's
//! own `list_dir("/")`:
//!
//! - **8 KiB requests**, what `ExfatSlotReader` issues today;
//! - **one request per cluster**, 32 KiB each;
//! - **physically contiguous clusters merged**, up to the 128 KiB transport maximum.
//!
//! Each of those repetitions starts a fresh SD session, so none is served by the session read cache
//! #196 added. The `list_dir` repetitions deliberately share one session: the first reads the card,
//! and the rest show what the cache saves.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example exfat_root_read_cost -- --port COM4
//! ```

use clap::Parser;
use multi64_sc64_sd::{CartSession, Sc64Link, Sc64SdSession};
use std::io;
use std::time::Instant;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    port: String,
    #[arg(long, default_value_t = 115200)]
    baud: u32,
    #[arg(long, default_value_t = 3)]
    reps: usize,
}

const MAX_REQUEST: usize = 128 * 1024;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

struct Root {
    /// Absolute LBA of each root cluster, in chain order.
    lbas: Vec<u64>,
    sectors_per_cluster: u64,
    chain_walk_secs: f64,
}

fn root_layout(link: &mut Sc64Link) -> io::Result<Root> {
    let mut s0 = vec![0u8; 512];
    link.read_sd_sectors(0, &mut s0)?;
    let part: u64 = if &s0[3..11] == b"EXFAT   " {
        0
    } else {
        (0..4)
            .map(|i| 446 + i * 16)
            .find(|&e| s0[e + 4] != 0 && le32(&s0, e + 8) != 0)
            .map(|e| u64::from(le32(&s0, e + 8)))
            .ok_or_else(|| io::Error::other("no partition"))?
    };
    let mut bs = vec![0u8; 512];
    link.read_sd_sectors(part, &mut bs)?;
    let fat_offset = u64::from(le32(&bs, 80));
    let heap_offset = u64::from(le32(&bs, 88));
    let cluster_count = le32(&bs, 92);
    let root = le32(&bs, 96);
    let spc = 1u64 << bs[109];

    // The same walk `ExfatDirSlots::load_with` does: one 4-byte FAT read per cluster, which the
    // disk layer turns into one sector request each.
    let t = Instant::now();
    let mut chain = vec![root];
    let mut c = root;
    let mut sector = vec![0u8; 512];
    loop {
        let byte = u64::from(c) * 4;
        link.read_sd_sectors(part + fat_offset + byte / 512, &mut sector)?;
        let next = le32(&sector, (byte % 512) as usize);
        if !(2..0xFFFF_FFF8).contains(&next) || next > cluster_count + 1 || chain.len() > 4096 {
            break;
        }
        chain.push(next);
        c = next;
    }
    Ok(Root {
        lbas: chain
            .iter()
            .map(|&c| part + heap_offset + (u64::from(c) - 2) * spc)
            .collect(),
        sectors_per_cluster: spc,
        chain_walk_secs: t.elapsed().as_secs_f64(),
    })
}

/// Read every root cluster with requests of at most `chunk` bytes, merging physically contiguous
/// clusters when `merge`. Returns (seconds, requests).
fn read_root(
    link: &mut Sc64Link,
    root: &Root,
    chunk: usize,
    merge: bool,
) -> io::Result<(f64, u32)> {
    let cluster_bytes = root.sectors_per_cluster as usize * 512;
    // Runs of physically contiguous clusters, as (first LBA, bytes).
    let mut runs: Vec<(u64, usize)> = Vec::new();
    for &lba in &root.lbas {
        match runs.last_mut() {
            Some((start, len)) if merge && *start + (*len / 512) as u64 == lba => {
                *len += cluster_bytes
            }
            _ => runs.push((lba, cluster_bytes)),
        }
    }
    let t = Instant::now();
    let mut requests = 0;
    let mut buf = vec![0u8; chunk];
    for (start, len) in runs {
        let mut done = 0;
        while done < len {
            let n = chunk.min(len - done);
            link.read_sd_sectors(start + (done / 512) as u64, &mut buf[..n])?;
            requests += 1;
            done += n;
        }
    }
    Ok((t.elapsed().as_secs_f64(), requests))
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    println!("exFAT root read cost (read-only)\n");

    {
        let mut link = Sc64Link::open(&args.port, args.baud)
            .map_err(|e| io::Error::other(format!("serial open: {e}")))?;
        link.identify()?;
        link.sd_init()?;
        let out = (|| -> io::Result<()> {
            let root = root_layout(&mut link)?;
            let kib = root.lbas.len() * root.sectors_per_cluster as usize * 512 / 1024;
            println!(
                "root: {} clusters, {kib} KiB; FAT chain walk {:.2}s ({} single-sector requests)\n",
                root.lbas.len(),
                root.chain_walk_secs,
                root.lbas.len()
            );
            let cluster_bytes = root.sectors_per_cluster as usize * 512;
            for (label, chunk, merge) in [
                ("8 KiB requests (today)", 8 * 1024, false),
                ("one request per cluster", cluster_bytes, false),
                ("contiguous clusters merged", MAX_REQUEST, true),
            ] {
                let mut best = f64::MAX;
                let mut reqs = 0;
                for _ in 0..args.reps {
                    // A fresh SD session per repetition: that clears the session read cache (#196),
                    // which would otherwise serve every repetition after the first.
                    link.sd_deinit_try();
                    link.sd_init()?;
                    let (s, r) = read_root(&mut link, &root, chunk, merge)?;
                    best = best.min(s);
                    reqs = r;
                }
                println!("  {label:<30} {reqs:>3} requests   best {best:.2}s");
            }
            Ok(())
        })();
        link.sd_deinit_try();
        out?;
    }

    println!();
    let session = CartSession::Sc64(Sc64SdSession::open(&args.port, args.baud)?);
    for i in 0..args.reps {
        let t = Instant::now();
        let n = session.list_dir("/")?.len();
        println!(
            "  session list_dir(\"/\") #{}{}: {n} entries in {:.2}s",
            i + 1,
            if i == 0 {
                " (reads the card)"
            } else {
                " (same session)"
            },
            t.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
