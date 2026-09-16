//! Read the card's exFAT root **directly off the media**, bypassing hadris entirely.
//!
//! Why this exists: the cart listed 74 root entries where the PC had written 1273. Two very
//! different things produce that, and neither the listing nor a path lookup can tell them apart,
//! because both resolve through the same hadris iterator that treats every exFAT root as
//! contiguous (`root_contiguous = true`, `root_size = 0`, `exfat/fs.rs:75`):
//!
//! - **The data is on the card** and our stack stops at the first root cluster. That is #175's
//!   read-side twin, and it means Xfer64 silently shows a fraction of a large card.
//! - **The writes never reached the media** (an unflushed card pulled from a reader). Then there is
//!   no bug here at all, just a bad test.
//!
//! So this parses the MBR and boot sector itself, walks the root's FAT chain itself, and decodes
//! the entry sets itself. Nothing it reports passes through `ExFatFs`.
//!
//! **Read-only.** It issues no writes. `sd_deinit` runs unconditionally, so the card is never left
//! locked to the PC side with the console unable to boot.
//!
//! ```sh
//! cargo run -p sc64-sd-e2e --release --example exfat_raw_root_walk -- --port COM4
//! ```

use clap::Parser;
use multi64_sc64_sd::Sc64Link;
use std::io;

#[derive(Parser, Debug)]
#[command(
    name = "exfat-raw-root-walk",
    about = "Walk the exFAT root's real cluster chain off the media, without hadris (read-only)"
)]
struct Args {
    /// Cart serial device (e.g. COM4).
    #[arg(long)]
    port: String,

    #[arg(long, default_value_t = 115200)]
    baud: u32,

    /// Report whether a name starting with this is present on the media.
    #[arg(long, default_value = "rdr01199")]
    expect: String,

    /// Stop walking the chain after this many clusters, in case the FAT is corrupt.
    #[arg(long, default_value_t = 512)]
    max_clusters: usize,
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn read_sectors(link: &mut Sc64Link, lba: u64, count: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; count * 512];
    link.read_sd_sectors(lba, &mut buf)?;
    Ok(buf)
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    println!("exFAT raw root walk (read-only, no hadris)");
    println!("  port  {} @ {}", args.port, args.baud);
    println!();

    let mut link = Sc64Link::open(&args.port, args.baud)
        .map_err(|e| io::Error::other(format!("serial open: {e}")))?;
    link.identify()?;
    link.sd_init()?;

    let out = walk(&mut link, &args);

    // Unconditional: an SD session left open keeps the card locked away from the console.
    link.sd_deinit_try();
    out
}

fn walk(link: &mut Sc64Link, args: &Args) -> io::Result<()> {
    // --- where the volume starts -------------------------------------------------------------
    let s0 = read_sectors(link, 0, 1)?;
    let part_start: u64 = if &s0[3..11] == b"EXFAT   " {
        println!("sector 0 is an exFAT boot sector (no partition table)");
        0
    } else {
        let mut found = 0u64;
        for i in 0..4 {
            let e = 446 + i * 16;
            let ptype = s0[e + 4];
            let lba = le32(&s0, e + 8) as u64;
            if ptype != 0 && lba != 0 {
                println!("MBR entry {i}: type 0x{ptype:02X}, starts at LBA {lba}");
                found = lba;
                break;
            }
        }
        found
    };

    let bs = read_sectors(link, part_start, 1)?;
    if &bs[3..11] != b"EXFAT   " {
        println!("no exFAT boot sector at LBA {part_start}; giving up.");
        return Ok(());
    }

    let fat_offset = le32(&bs, 80) as u64;
    let fat_length = le32(&bs, 84) as u64;
    let heap_offset = le32(&bs, 88) as u64;
    let cluster_count = le32(&bs, 92);
    let root_cluster = le32(&bs, 96);
    let bytes_per_sector = 1u64 << bs[108];
    let sectors_per_cluster = 1u64 << bs[109];
    let fat_count = bs[110];
    let cluster_bytes = (bytes_per_sector * sectors_per_cluster) as usize;
    let slots_per_cluster = cluster_bytes / 32;

    println!("\n=== volume");
    println!("  partition start     LBA {part_start}");
    println!("  bytes/sector        {bytes_per_sector}");
    println!("  sectors/cluster     {sectors_per_cluster}");
    println!(
        "  cluster size        {} KiB ({slots_per_cluster} entry slots)",
        cluster_bytes / 1024
    );
    println!("  FAT at              LBA +{fat_offset}, {fat_length} sectors, {fat_count} copies");
    println!("  cluster heap at     LBA +{heap_offset}, {cluster_count} clusters");
    println!("  root cluster        {root_cluster}");
    println!("  fs revision         {}.{}", bs[105], bs[104]);
    println!("  volume flags        0x{:04X}", le16(&bs, 106));

    // --- the root's real cluster chain, straight out of the FAT -------------------------------
    let cluster_lba = |c: u32| part_start + heap_offset + (u64::from(c) - 2) * sectors_per_cluster;

    let next_cluster = |link: &mut Sc64Link, c: u32| -> io::Result<Option<u32>> {
        let byte = u64::from(c) * 4;
        let lba = part_start + fat_offset + byte / bytes_per_sector;
        let off = (byte % bytes_per_sector) as usize;
        let sector = read_sectors(link, lba, 1)?;
        let v = le32(&sector, off);
        // Cluster numbers start at 2; 0xFFFFFFF7 is a bad cluster and >= 0xFFFFFFF8 is
        // end-of-chain. The cluster_count bound is separate on purpose: it is what stops a corrupt
        // FAT from walking us outside the cluster heap.
        if !(2..0xFFFF_FFF8).contains(&v) || v > cluster_count + 1 {
            Ok(None)
        } else {
            Ok(Some(v))
        }
    };

    let mut chain = vec![root_cluster];
    let mut c = root_cluster;
    while chain.len() < args.max_clusters {
        match next_cluster(link, c)? {
            Some(n) => {
                if chain.contains(&n) {
                    println!("  (FAT chain loops back to cluster {n}; stopping)");
                    break;
                }
                chain.push(n);
                c = n;
            }
            None => break,
        }
    }

    println!("\n=== root cluster chain");
    println!("  {} cluster(s) in the chain", chain.len());
    let show: Vec<String> = chain.iter().take(12).map(|c| c.to_string()).collect();
    println!(
        "  {}{}",
        show.join(" -> "),
        if chain.len() > 12 { " -> …" } else { "" }
    );
    let contiguous = chain.windows(2).all(|w| w[1] == w[0] + 1);
    println!("  physically contiguous: {contiguous}");
    if chain.len() > 1 && !contiguous {
        println!("  NOTE  a contiguous reader would walk into the wrong clusters after the first");
    }

    // --- decode the entry sets ourselves -------------------------------------------------------
    let mut all = Vec::with_capacity(chain.len() * cluster_bytes);
    for &cl in &chain {
        all.extend_from_slice(&read_sectors(
            link,
            cluster_lba(cl),
            sectors_per_cluster as usize,
        )?);
    }

    let mut names: Vec<String> = Vec::new();
    let mut end_marker: Option<usize> = None;
    let mut i = 0usize;
    while i + 32 <= all.len() {
        let t = all[i];
        if t == 0x00 {
            end_marker = Some(i);
            break;
        }
        if t == 0x85 {
            let secondaries = all[i + 1] as usize;
            let name_len = if i + 32 + 4 <= all.len() {
                all[i + 35] as usize
            } else {
                0
            };
            let mut units: Vec<u16> = Vec::new();
            for k in 0..secondaries.saturating_sub(1) {
                let off = i + 64 + k * 32;
                if off + 32 > all.len() || all[off] != 0xC1 {
                    continue;
                }
                for j in 0..15 {
                    units.push(le16(&all, off + 2 + j * 2));
                }
            }
            units.truncate(name_len);
            names.push(String::from_utf16_lossy(&units));
            i += 32 * (1 + secondaries);
            continue;
        }
        i += 32;
    }

    println!("\n=== entry sets found on the media");
    println!("  {} in-use names decoded", names.len());
    let rdr = names.iter().filter(|n| n.starts_with("rdr")).count();
    let fill = names.iter().filter(|n| n.starts_with("fill")).count();
    println!("  rdr* : {rdr}");
    println!("  fill*: {fill}");
    println!("  other: {}", names.len() - rdr - fill);
    let wanted = names.iter().any(|n| n.starts_with(&args.expect));
    println!("  a name starting `{}` present: {wanted}", args.expect);
    match end_marker {
        Some(b) => println!(
            "  end-of-directory marker at byte {b} — cluster {} of {} in the chain",
            b / cluster_bytes + 1,
            chain.len()
        ),
        None => println!("  no end-of-directory marker inside the chain"),
    }

    // --- the verdict this tool exists to give --------------------------------------------------
    println!("\n=== verdict");
    if rdr > 100 {
        println!(
            "  The reader's files ARE on the media: {rdr} of them, across {} root clusters.",
            chain.len()
        );
        println!("  The cart's own listing showed 1. So multi64's listing truncates a chained");
        println!("  root — the read-side twin of #175, reproduced on hardware.");
    } else if rdr <= 2 {
        println!("  The reader's files are NOT on the media ({rdr} found).");
        println!("  The writes never flushed before the card was pulled, so the short listing was");
        println!("  a test artifact, not a multi64 bug. Refill the card and eject it safely.");
    } else {
        println!(
            "  Inconclusive: {rdr} rdr* names on the media. Neither explanation fits cleanly."
        );
    }
    Ok(())
}
