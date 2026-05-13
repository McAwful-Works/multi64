//! End-to-end: build **L3 frames** (`M64B` wire), send the encoded stream over [`Sc64L2Pipe`], read the echoed bytes, decode with [`multi64_l3::Frame::decode`].
//!
//! Requires **`multi64_test.z64`** in **RAW_ECHO** mode (default) on SummerCart64 — it echoes `MULTI64_L3` octets verbatim. Use **`--large`** to split a big payload across multiple USB chunks.

use clap::Parser;
use multi64_l3::{Channel, Frame, FrameFlags, FrameType};
use multi64_sc64_l2::Sc64L2Pipe;
use std::time::Duration;

/// Slightly larger than one `USB_WRITE` L3 stream chunk (8192) so the wire spans two host chunks.
const LARGE_PAYLOAD_LEN: usize = 8192 + 100;

#[derive(Parser, Debug)]
#[command(name = "sc64-l3-framing-e2e")]
struct Args {
    #[arg(short, long)]
    port: String,

    #[arg(long, default_value = "115200")]
    baud: u32,

    #[arg(long, default_value = "10")]
    timeout_secs: u64,

    /// Also run a large `DATA` frame that spans multiple `USB_WRITE` chunks (8192-byte stream chunks).
    #[arg(long, default_value = "false")]
    large: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let deadline = Duration::from_secs(args.timeout_secs.max(1));

    let mut pipe = Sc64L2Pipe::open(&args.port, args.baud)?;
    pipe.set_timeout(Duration::from_millis(100))?;
    pipe.clear_serial_buffers()?;

    run_case(
        &mut pipe,
        "small DATA frame",
        Frame {
            ty: FrameType::Data,
            channel: Channel::Application,
            flags: FrameFlags::FINAL,
            request_id: 0x11223344,
            payload: b"l3 framing e2e".to_vec(),
        },
        deadline,
    )?;

    run_case(
        &mut pipe,
        "HEARTBEAT (zero payload)",
        Frame {
            ty: FrameType::Heartbeat,
            channel: Channel::Control,
            flags: FrameFlags::empty(),
            request_id: 0,
            payload: Vec::new(),
        },
        deadline,
    )?;

    if args.large {
        let payload: Vec<u8> = (0..LARGE_PAYLOAD_LEN).map(|i| (i & 0xFF) as u8).collect();
        run_case(
            &mut pipe,
            "large DATA frame (multi-chunk USB)",
            Frame {
                ty: FrameType::Data,
                channel: Channel::Application,
                flags: FrameFlags::FINAL,
                request_id: 0xAABBCCDD,
                payload,
            },
            deadline,
        )?;
    }

    println!("OK: all L3 framing e2e checks passed");
    Ok(())
}

fn run_case(
    pipe: &mut Sc64L2Pipe,
    label: &str,
    frame: Frame,
    deadline: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let wire = frame.encode()?;
    println!(
        "{label}: wire_len={} payload_len={}",
        wire.len(),
        frame.payload.len()
    );

    pipe.write_l3_stream(&wire)?;

    let mut back = vec![0u8; wire.len()];
    pipe.read_l3_bytes_exact(&mut back, deadline)?;

    if back != wire {
        return Err(format!("{label}: wire bytes mismatch").into());
    }

    let (decoded, consumed) = Frame::decode(&back)?;
    if consumed != back.len() {
        return Err(format!("{label}: decode consumed {consumed} != {}", back.len()).into());
    }
    if decoded.ty != frame.ty
        || decoded.channel != frame.channel
        || decoded.flags != frame.flags
        || decoded.request_id != frame.request_id
        || decoded.payload != frame.payload
    {
        return Err(format!("{label}: decoded frame does not match original").into());
    }

    Ok(())
}
