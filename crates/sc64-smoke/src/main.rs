//! Minimal **host tool** for SummerCart64 vendor commands: open the serial port, send
//! **`IDENTIFIER_GET`** and **`VERSION_GET`**, print results. Three modes go further:
//!
//! - **`--boot-rom <ROM>`** boots a `.z64` with the cart's own bootloader instead of its menu: the
//!   ROM is written into SDRAM (`MEMORY_WRITE`), read back and compared (`MEMORY_READ`), and config
//!   `BOOT_MODE` set to `1`, so the next console Reset starts that ROM. `CIC_SEED` is left as it is;
//!   at its default, `0xFFFF`, the bootloader picks the CIC from the ROM's IPL3. Use it when the
//!   SC64 menu will not boot a ROM, as on 2026-10-08, when it hung on a black screen for every
//!   libdragon ROM while this path booted them (`n64/README.md`, "Loading a ROM onto a
//!   SummerCart64").
//! - **`--boot-menu`** sets `BOOT_MODE` back to `0`. The cart keeps `BOOT_MODE` across console
//!   resets, and across power cycles while USB powers it (vendor `docs/04_config_options.md`), so
//!   until this runs every boot starts the ROM in SDRAM and the menu never appears.
//! - **`--config`** prints every config value. Read-only.
//!
//! If `multi64d` answers at `--daemon`, it is released before the port opens and resumed after,
//! in every mode (see [`daemon`]).
//!
//! ```sh
//! cargo run -p sc64-smoke --release -- --port COM4 --boot-rom n64/bringup/multi64_bringup.z64
//! cargo run -p sc64-smoke --release -- --port COM4 --boot-menu
//! ```
//!
//! Does not use L3 framing; see **`sc64-echo-test`** / **`sc64-l3-framing-e2e`** for L3 paths
//! (with **`multi64_test.z64`** in **RAW_ECHO** mode). Vendor commands: SummerCart64
//! **`docs/03_usb_interface.md`**; config ids **`docs/04_config_options.md`**.

mod boot;
mod daemon;

use clap::Parser;
use multi64_sc64_link::{cmd, cmd_packet, CmpResponse, ResponseBuffer};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "sc64-smoke",
    about = "SummerCart64 USB serial smoke test and direct ROM boot"
)]
struct Args {
    /// Serial device (e.g. COM5 on Windows, /dev/ttyACM0 on Linux)
    #[arg(short, long)]
    port: String,

    /// Baud rate (SC64 uses USB-CDC; value is often ignored but required by APIs)
    #[arg(long, default_value = "115200")]
    baud: u32,

    /// Optional DTR reset sequence before talking (see SC64 USB docs)
    #[arg(long, default_value = "false")]
    reset: bool,

    /// Wall-clock timeout while waiting for each CMP response
    #[arg(long, default_value = "5000")]
    timeout_ms: u64,

    /// Write this .z64 into the cart's SDRAM, check it, and set BOOT_MODE 1 so the next console
    /// Reset boots it with the cart's bootloader instead of the menu. Undo with --boot-menu.
    #[arg(long, value_name = "ROM", conflicts_with = "boot_menu")]
    boot_rom: Option<PathBuf>,

    /// Set BOOT_MODE back to 0, so the next console Reset loads the menu again.
    #[arg(long)]
    boot_menu: bool,

    /// Print every cart config value. Read-only.
    #[arg(long)]
    config: bool,

    /// multi64d's address. If it answers there, it is released for the run and resumed after.
    #[arg(long, default_value = "http://127.0.0.1:38765")]
    daemon: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let pause = match daemon::pause(&args.daemon) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // `run` owns the port, so it is closed before multi64d reopens it.
    let result = run(&args);
    let resumed = pause.resume();
    let mut code = ExitCode::SUCCESS;
    if let Err(e) = result {
        eprintln!("error: {e}");
        code = ExitCode::FAILURE;
    }
    if let Err(e) = resumed {
        eprintln!("error: {e}");
        code = ExitCode::FAILURE;
    }
    code
}

fn run(args: &Args) -> io::Result<()> {
    let mut port = serialport::new(&args.port, args.baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| io::Error::other(format!("{}: {e}", args.port)))?;
    let port = &mut *port;
    let timeout = Duration::from_millis(args.timeout_ms);

    if args.reset {
        dtr_reset(port)?;
    }

    let ident = send_and_read_cmp(
        port,
        &cmd_packet(cmd::IDENTIFIER_GET, 0, 0, &[]),
        cmd::IDENTIFIER_GET,
        timeout,
    )?;
    let ident = String::from_utf8_lossy(&ident.data)
        .trim_end_matches('\0')
        .to_string();
    println!("identifier: {ident}");

    let ver = send_and_read_cmp(
        port,
        &cmd_packet(cmd::VERSION_GET, 0, 0, &[]),
        cmd::VERSION_GET,
        timeout,
    )?;
    if ver.data.len() >= 8 {
        let major = u16::from_be_bytes([ver.data[0], ver.data[1]]);
        let minor = u16::from_be_bytes([ver.data[2], ver.data[3]]);
        let rev = u32::from_be_bytes(ver.data[4..8].try_into().unwrap());
        println!("firmware: {major}.{minor} (rev {rev})");
    } else {
        println!("firmware: (unexpected response length {})", ver.data.len());
    }

    if args.config {
        for (id, name) in boot::CONFIG_NAMES.iter().enumerate() {
            let value = config_get(port, id as u32, timeout)?;
            println!("{id:>2} {name:<20} {value:#010X}");
        }
    }

    // Writing SDRAM or BOOT_MODE on anything else would be a guess at its memory map.
    if (args.boot_rom.is_some() || args.boot_menu) && ident != "SCv2" {
        return Err(io::Error::other(format!(
            "not a SummerCart64 (identifier {ident:?}, expected \"SCv2\"); nothing written"
        )));
    }
    if let Some(rom) = &args.boot_rom {
        boot_rom(port, rom, &args.port, timeout)?;
    }
    if args.boot_menu {
        let before = config_get(port, boot::CONFIG_BOOT_MODE, timeout)?;
        set_boot_mode(port, boot::BOOT_MODE_MENU, timeout)?;
        println!(
            "BOOT_MODE: {before} ({}) -> {} ({}). The next console Reset loads the menu.",
            boot::boot_mode_name(before),
            boot::BOOT_MODE_MENU,
            boot::boot_mode_name(boot::BOOT_MODE_MENU)
        );
    }
    Ok(())
}

fn boot_rom(
    port: &mut dyn serialport::SerialPort,
    path: &Path,
    port_name: &str,
    timeout: Duration,
) -> io::Result<()> {
    let rom = std::fs::read(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    if let Some(why) = boot::check_rom(&rom) {
        return Err(io::Error::other(format!(
            "{}: {why}; nothing written",
            path.display()
        )));
    }
    let title = String::from_utf8_lossy(&rom[0x20..0x34])
        .trim_end_matches(['\0', ' '])
        .to_string();
    println!("ROM: {} ({} bytes, {title:?})", path.display(), rom.len());

    let before = config_get(port, boot::CONFIG_BOOT_MODE, timeout)?;
    let cic = config_get(port, boot::CONFIG_CIC_SEED, timeout)?;

    // Each chunk is one command and one response; never wait less than this for one.
    let chunk_timeout = timeout.max(Duration::from_secs(30));

    let t = Instant::now();
    let mut chunks = 0;
    for (addr, pkt) in boot::memory_write_packets(&rom) {
        send_and_read_cmp(port, &pkt, cmd::MEMORY_WRITE, chunk_timeout)
            .map_err(|e| io::Error::new(e.kind(), format!("MEMORY_WRITE at {addr:#X}: {e}")))?;
        chunks += 1;
    }
    println!(
        "wrote: {} bytes to SDRAM at 0 in {chunks} chunk(s), {} ms",
        rom.len(),
        t.elapsed().as_millis()
    );

    let t = Instant::now();
    let mut back = Vec::with_capacity(rom.len());
    for (addr, len, pkt) in boot::memory_read_packets(rom.len()) {
        let r = send_and_read_cmp(port, &pkt, cmd::MEMORY_READ, chunk_timeout)
            .map_err(|e| io::Error::new(e.kind(), format!("MEMORY_READ at {addr:#X}: {e}")))?;
        if r.data.len() != len {
            return Err(io::Error::other(format!(
                "MEMORY_READ at {addr:#X}: asked for {len} bytes, got {}",
                r.data.len()
            )));
        }
        back.extend_from_slice(&r.data);
    }
    if let Some((first, count)) = boot::first_difference(&rom, &back) {
        return Err(io::Error::other(format!(
            "SDRAM does not match the ROM: {count} bytes differ, the first at {first:#X}. \
             BOOT_MODE left at {before}"
        )));
    }
    println!(
        "verified: SDRAM read back byte for byte, {} ms",
        t.elapsed().as_millis()
    );

    set_boot_mode(port, boot::BOOT_MODE_ROM, timeout)?;
    println!(
        "BOOT_MODE: {before} ({}) -> {} ({})",
        boot::boot_mode_name(before),
        boot::BOOT_MODE_ROM,
        boot::boot_mode_name(boot::BOOT_MODE_ROM)
    );
    if cic == boot::CIC_SEED_AUTO {
        println!("CIC_SEED: 0xFFFF, so the bootloader picks the CIC from the ROM's IPL3");
    } else {
        println!(
            "CIC_SEED: {cic:#06X}, not 0xFFFF (automatic): the bootloader uses that, not the ROM's \
             IPL3. Left as it is."
        );
    }
    println!();
    println!("Press Reset on the console to boot this ROM.");
    println!("The cart keeps BOOT_MODE 1 across resets, and skips the menu until you run:");
    println!("  sc64-smoke --port {port_name} --boot-menu");
    Ok(())
}

fn config_get(
    port: &mut dyn serialport::SerialPort,
    id: u32,
    timeout: Duration,
) -> io::Result<u32> {
    let r = send_and_read_cmp(port, &boot::config_get_packet(id), cmd::CONFIG_GET, timeout)?;
    boot::config_value(&r.data).ok_or_else(|| {
        io::Error::other(format!(
            "CONFIG_GET {id}: expected 4 bytes, got {}",
            r.data.len()
        ))
    })
}

/// Set `BOOT_MODE` and read it back, so a value the cart did not take is an error.
fn set_boot_mode(
    port: &mut dyn serialport::SerialPort,
    value: u32,
    timeout: Duration,
) -> io::Result<()> {
    send_and_read_cmp(
        port,
        &boot::config_set_packet(boot::CONFIG_BOOT_MODE, value),
        cmd::CONFIG_SET,
        timeout,
    )?;
    let now = config_get(port, boot::CONFIG_BOOT_MODE, timeout)?;
    if now != value {
        return Err(io::Error::other(format!(
            "BOOT_MODE set to {value} but reads back {now}"
        )));
    }
    Ok(())
}

/// SC64 USB docs: DTR/DSR emulated reset. Best-effort: some backends ignore modem lines.
fn dtr_reset(port: &mut dyn serialport::SerialPort) -> io::Result<()> {
    port.write_data_terminal_ready(true)?;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if port.read_data_set_ready()? {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    port.clear(serialport::ClearBuffer::All)?;
    port.write_data_terminal_ready(false)?;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !port.read_data_set_ready()? {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn send_and_read_cmp(
    port: &mut dyn serialport::SerialPort,
    cmd: &[u8],
    expect_cmd: u8,
    total_timeout: Duration,
) -> io::Result<CmpResponse> {
    port.write_all(cmd)?;
    port.flush()?;

    let mut buf = ResponseBuffer::default();
    let start = Instant::now();
    // Large enough that a 1 MiB MEMORY_READ is not thousands of reads.
    let mut scratch = vec![0u8; 64 * 1024];

    while start.elapsed() < total_timeout {
        match port.read(&mut scratch) {
            Ok(0) => {}
            Ok(n) => buf.push_bytes(&scratch[..n]),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }
        while let Some(r) = buf.next_cmp() {
            if r.cmd_id == expect_cmd {
                if r.ok {
                    return Ok(r);
                }
                return Err(io::Error::other("device returned ERR for command"));
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "timed out waiting for CMP response",
    ))
}
