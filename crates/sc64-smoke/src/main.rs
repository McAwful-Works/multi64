//! Minimal **host smoke test** for SummerCart64: open the serial port, send **`IDENTIFIER_GET`** and **`VERSION_GET`**, print results.
//!
//! Does not use L3 framing; see **`sc64-echo-test`** / **`sc64-l3-framing-e2e`** for L3 paths (with **`multi64_test.z64`** in **RAW_ECHO** mode). Vendor commands: SummerCart64 **`docs/03_usb_interface.md`** (`v` / `V`).

use clap::Parser;
use multi64_sc64_link::{cmd, cmd_packet, CmpResponse, ResponseBuffer};
use std::io;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "sc64-smoke", about = "SummerCart64 USB serial smoke test")]
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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut port = serialport::new(&args.port, args.baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| io::Error::other(format!("{e}")))?;

    if args.reset {
        // SC64 USB docs: DTR/DSR emulated reset. Best-effort: some backends ignore modem lines.
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
    }

    let ident = send_and_read_cmp(
        &mut *port,
        &cmd_packet(cmd::IDENTIFIER_GET, 0, 0, &[]),
        cmd::IDENTIFIER_GET,
        Duration::from_millis(args.timeout_ms),
    )?;
    println!(
        "identifier: {}",
        String::from_utf8_lossy(&ident.data).trim_end_matches('\0')
    );

    let ver = send_and_read_cmp(
        &mut *port,
        &cmd_packet(cmd::VERSION_GET, 0, 0, &[]),
        cmd::VERSION_GET,
        Duration::from_millis(args.timeout_ms),
    )?;
    if ver.data.len() >= 8 {
        let major = u16::from_be_bytes([ver.data[0], ver.data[1]]);
        let minor = u16::from_be_bytes([ver.data[2], ver.data[3]]);
        let rev = u32::from_be_bytes(ver.data[4..8].try_into().unwrap());
        println!("firmware: {major}.{minor} (rev {rev})");
    } else {
        println!("firmware: (unexpected response length {})", ver.data.len());
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
    let mut scratch = [0u8; 256];

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
        std::thread::sleep(Duration::from_millis(1));
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "timed out waiting for CMP response",
    ))
}
