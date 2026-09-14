//! End-to-end: build **L3 frames** (`M64B` wire), send the encoded stream over [`Ed64ProL2Pipe`], read the echoed bytes, decode with [`multi64_l3::Frame::decode`].
//!
//! Requires **`multi64_test.z64`** in **RAW_ECHO** mode (default) on an **EverDrive-64 PRO**.
//! **`multi64-ed64pro-l2`** implements **`docs/spec/l3-over-everdrive-pro.md`**, this repository's own design, and
//! has never been run against a cart, so a failure here may be the mapping rather than the ROM. See
//! [spec §8–§9](../../docs/spec/l3-over-everdrive-pro.md).
//!
//! Use **`--large`** for a frame that spans several 1024-byte FIFO writes. The cart's FIFO holds 2048 bytes and
//! gives the host no drain feedback, so this is the case that probes the spacing in spec §5.

use clap::Parser;
use multi64_ed64pro_l2::Ed64ProL2Pipe;
use multi64_ed64pro_link::Ed64Pro;
use multi64_l3::{Channel, Frame, FrameFlags, FrameType};
use std::time::Duration;

/// Slightly larger than one typical host L3 chunk, so the wire spans nine 1024-byte FIFO writes.
const LARGE_PAYLOAD_LEN: usize = 8192 + 100;

#[derive(Parser, Debug)]
#[command(name = "ed64pro-l3-framing-e2e")]
struct Args {
    /// The PRO's COM port. It always runs at 921600 baud, so there is no `--baud`.
    #[arg(short, long)]
    port: String,

    #[arg(long, default_value = "10")]
    timeout_secs: u64,

    /// Also run a large `DATA` frame that spans several FIFO writes.
    #[arg(long, default_value = "false")]
    large: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let deadline = Duration::from_secs(args.timeout_secs.max(1));

    let mut dev = Ed64Pro::open(&args.port)?;
    let id = dev.identity()?;
    println!(
        "EverDrive-64 PRO answered on {}: protocol 0x{:02X}, device 0x{:02X}",
        args.port, id.protocol_id, id.device_id
    );

    let mut pipe = Ed64ProL2Pipe::from_device(dev)?;
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
            "large DATA frame (several FIFO writes)",
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
    pipe: &mut Ed64ProL2Pipe,
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
