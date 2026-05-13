//! Minimal **host smoke test** for **EverDrive-64** USB serial: send a **Krikzz `usb64`-style**
//! **test connection** packet (`cmd` + `t` + address/length/argument fields), read the response,
//! print **OK** / **region** when the device answers in the **USB64** style.
//!
//! This does **not** use L3 framing. The outbound layout matches
//! **`usb64/usb64/CommandProcessor.cs`** in [krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub)
//! (ASCII `cmd`, command byte `t`, then three big-endian `uint32` fields — zeros for this probe).
//! Some **older community** loaders used a **4-byte** uppercase **`CMD`** + **`T`** opener only;
//! that form is **not** what this binary sends (see [`docs/spec/l3-over-everdrive-x7.md`](../../../docs/spec/l3-over-everdrive-x7.md) §8).
//! **Firmware and X7 OS builds vary** — if this fails, try another **`--baud`** or confirm the cart
//! exposes a serial port with the EverDrive USB protocol active (often from the menu / OS).

use clap::Parser;
use std::io;
use std::time::{Duration, Instant};

/// Krikzz `usb64` `CommandPacketTransmit(TransmitCommand.TestConnection)` with defaults (all-zero tail).
fn test_connection_packet() -> [u8; 16] {
    let mut p = [0u8; 16];
    p[0..3].copy_from_slice(b"cmd");
    p[3] = b't';
    p
}

#[derive(Parser, Debug)]
#[command(
    name = "ed64-smoke",
    about = "EverDrive USB serial smoke test (usb64-style cmd/t test connection)"
)]
struct Args {
    /// Serial device (e.g. COM5 on Windows, /dev/ttyUSB0 on Linux)
    #[arg(short, long)]
    port: String,

    /// Baud rate (EverDrive USB serial varies by OS/driver; try 115200 or 57600)
    #[arg(long, default_value = "115200")]
    baud: u32,

    /// Discard inbound bytes before sending the test command
    #[arg(long, default_value = "false")]
    flush: bool,

    /// Wall-clock timeout for the full read after sending the test packet
    #[arg(long, default_value = "3000")]
    timeout_ms: u64,

    /// Print raw hex of the first bytes received
    #[arg(long, default_value = "false")]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut port = serialport::new(&args.port, args.baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| io::Error::other(format!("{e}")))?;

    if args.flush {
        let _ = port.clear(serialport::ClearBuffer::Input);
    }

    let pkt = test_connection_packet();
    port.write_all(&pkt)?;
    port.flush()?;

    let deadline = Duration::from_millis(args.timeout_ms);
    let buf = read_up_to(&mut *port, 512, deadline)?;

    if args.verbose && !buf.is_empty() {
        let n = buf.len().min(64);
        println!("raw[0..{}]: {}", n, hex_fmt(&buf[..n]));
    }

    match parse_test_response(&buf) {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

fn read_up_to(
    port: &mut dyn serialport::SerialPort,
    max: usize,
    total_timeout: Duration,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut scratch = [0u8; 256];
    let start = Instant::now();

    while start.elapsed() < total_timeout && out.len() < max {
        match port.read(&mut scratch) {
            Ok(0) => std::thread::sleep(Duration::from_millis(1)),
            Ok(n) => {
                out.extend_from_slice(&scratch[..n]);
                if out.len() >= 4 && reply_byte_ok(out[3]) {
                    // Give the device a short window for trailing bytes (region code, etc.).
                    let t0 = Instant::now();
                    while t0.elapsed() < Duration::from_millis(80) && out.len() < max {
                        match port.read(&mut scratch) {
                            Ok(0) => break,
                            Ok(n) => out.extend_from_slice(&scratch[..n]),
                            Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
                            Err(e) => return Err(e),
                        }
                    }
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }
    }

    Ok(out)
}

fn reply_byte_ok(b: u8) -> bool {
    matches!(b, b'k' | b'r')
}

fn parse_test_response(buf: &[u8]) -> Result<String, String> {
    if buf.is_empty() {
        return Err(
            "no bytes received (wrong port, baud, or EverDrive USB not ready). \
             Try --flush, another --baud (57600/115200), or OS menu with USB active."
                .into(),
        );
    }
    if buf.len() < 4 {
        return Err(format!(
            "short response ({} bytes): {}",
            buf.len(),
            hex_fmt(buf)
        ));
    }
    if !reply_byte_ok(buf[3]) {
        return Err(format!(
            "unexpected response (expected byte[3] = 'k' (legacy) or 'r' (reply): {}",
            hex_fmt(&buf[..buf.len().min(32)])
        ));
    }

    if buf.len() > 4 && buf[4] == b'3' {
        let region = buf
            .get(5)
            .map(|b| match *b {
                b'p' => "PAL",
                b'n' => "NTSC",
                b'm' => "MPAL",
                _ => "unknown",
            })
            .unwrap_or("unknown");
        return Ok(format!(
            "EverDrive USB handshake OK (OS-style reply with region hint: {region})"
        ));
    }

    Ok("EverDrive USB handshake OK (cmd/t)".into())
}

fn hex_fmt(slice: &[u8]) -> String {
    slice
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_packet_matches_usb64_defaults() {
        let p = test_connection_packet();
        assert_eq!(&p[0..4], b"cmdt");
        assert!(p[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_min_k_at_three() {
        let buf = [0u8, 0u8, 0u8, b'k'];
        assert!(parse_test_response(&buf).is_ok());
    }

    #[test]
    fn parse_r_at_three() {
        let buf = [b'c', b'm', b'd', b'r', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(parse_test_response(&buf).is_ok());
    }

    #[test]
    fn parse_rsp_style() {
        let buf = *b"RSPk";
        assert!(parse_test_response(&buf).is_ok());
    }

    #[test]
    fn parse_v3_ntsc() {
        let buf = *b"RSPk3n";
        let s = parse_test_response(&buf).unwrap();
        assert!(s.contains("NTSC"), "{s}");
    }
}
