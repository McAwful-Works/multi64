//! Send a short **raw L3 octet** payload through [`Ed64ProL2Pipe`] and compare with the echoed bytes (no `M64B` framing).
//!
//! Run **`multi64_test.z64`** in **RAW_ECHO** mode (default) on an EverDrive-64 PRO, then:
//! `cargo run -p ed64pro-echo-test -- --port COM3`
//!
//! **Experimental.** `multi64-ed64pro-l2` is this repository's own design for the PRO and has never been run
//! against a cart, so a failure here is as likely to be the mapping as the ROM. Opening the port runs the edlink
//! handshake, so a port with no PRO behind it fails before anything is sent — but a passing handshake only shows
//! that a PRO answered, not that a running ROM receives its FIFO. See
//! [spec §8–§9](../../docs/spec/l3-over-everdrive-pro.md).

use clap::Parser;
use multi64_ed64pro_l2::Ed64ProL2Pipe;
use multi64_ed64pro_link::Ed64Pro;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "ed64pro-echo-test")]
struct Args {
    /// The PRO's COM port. It always runs at 921600 baud, so there is no `--baud`.
    #[arg(short, long)]
    port: String,

    /// Payload to send. `Ed64ProL2Pipe::write_l3_stream` splits it into FIFO writes of at most 1024 bytes.
    #[arg(long, default_value = "multi64_test")]
    payload: String,

    /// Total wait for the echoed bytes
    #[arg(long, default_value = "10")]
    timeout_secs: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let payload = args.payload.as_bytes();
    if payload.is_empty() {
        return Err("empty payload".into());
    }

    let mut dev = Ed64Pro::open(&args.port)?;
    let id = dev.identity()?;
    println!(
        "EverDrive-64 PRO answered on {}: protocol 0x{:02X}, device 0x{:02X}",
        args.port, id.protocol_id, id.device_id
    );

    let mut pipe = Ed64ProL2Pipe::from_device(dev)?;
    pipe.set_timeout(Duration::from_millis(100))?;
    pipe.clear_serial_buffers()?;

    pipe.write_l3_stream(payload)?;

    let mut back = vec![0u8; payload.len()];
    pipe.read_l3_bytes_exact(&mut back, Duration::from_secs(args.timeout_secs.max(1)))?;

    if back == payload {
        println!("OK: echoed {} bytes", payload.len());
        Ok(())
    } else {
        eprintln!("mismatch: sent {:?}, got {:?}", payload, back);
        std::process::exit(1);
    }
}
