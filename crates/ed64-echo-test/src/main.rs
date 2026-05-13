//! Send a short **raw L3 octet** payload through [`Ed64L2Pipe`] and compare with the echoed bytes (no `M64B` framing).
//!
//! Run **`multi64_test.z64`** in **RAW_ECHO** mode (default), then: `cargo run -p ed64-echo-test -- --port COM3`
//!
//! **`multi64-ed64-l2`** must implement the EverDrive wire mapping — until then [`Ed64L2Pipe::open`] fails with
//! [`std::io::ErrorKind::Unsupported`].

use clap::Parser;
use multi64_ed64_l2::Ed64L2Pipe;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "ed64-echo-test")]
struct Args {
    #[arg(short, long)]
    port: String,

    #[arg(long, default_value = "115200")]
    baud: u32,

    /// Payload to send (length limits depend on the EverDrive L2 mapping once implemented)
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

    let mut pipe = Ed64L2Pipe::open(&args.port, args.baud)?;
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
