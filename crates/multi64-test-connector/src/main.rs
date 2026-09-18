//! CLI for **`multi64_test.z64`** over **`multi64d`** WebSocket — see [`docs/connectors/test-rom.md`](../../docs/connectors/test-rom.md).

use anyhow::Result;
use clap::{Parser, Subcommand};
use multi64_test_connector::suite::{run_suite, CheckResult, Outcome, SuiteOptions};
use multi64_test_connector::{
    run_connector_command, run_controller_poll, run_listen, ConnectorCommand,
};

fn note_suffix(note: &Option<String>) -> String {
    match note {
        Some(n) => format!("  ({n})"),
        None => String::new(),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "multi64-test-connector",
    about = "n64/test-rom ↔ multi64d WebSocket (L3 APPLICATION / M64T). Run multi64d with multi64_test.z64 (M64T_PROTO/BENCH, or CTRL_POLL for host-driven REQ_CONTROLLER)."
)]
struct Args {
    /// WebSocket URL (multi64d `/ws`).
    #[arg(long, global = true, default_value = "ws://127.0.0.1:38765/ws")]
    url: String,

    /// Max seconds to wait for an expected reply (M64T request/response commands).
    #[arg(long, global = true, default_value_t = 5.0)]
    recv_timeout_secs: f64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Ping,
    Echo {
        #[arg(long, group = "payload")]
        hex: Option<String>,
        #[arg(long, group = "payload")]
        text: Option<String>,
    },
    Version,
    ReqController,
    SessionOpen {
        #[arg(long, default_value = "0000000000000000")]
        hex_challenge: String,
    },
    SessionClose,
    EepromInfo,
    EepromRead {
        #[arg(long)]
        offset: u16,
        #[arg(long)]
        len: u16,
    },
    EepromWrite {
        #[arg(long)]
        offset: u16,
        #[arg(long)]
        hex: String,
    },
    SramInfo,
    SramRead {
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        len: u16,
    },
    SramWrite {
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        hex: String,
    },
    Rumble {
        #[arg(long, default_value_t = 0)]
        port: u8,
        #[arg(long, default_value_t = 60)]
        frames: u8,
    },
    DisplayText {
        #[arg(long, default_value = "")]
        text: String,
    },
    /// Put the ROM into a mode. Works from any mode, RAW_ECHO included.
    SetMode {
        /// 0 RAW_ECHO, 1 M64T_PROTO, 2 BENCH, 3 CTRL_POLL, 4 MEM_AGENT.
        #[arg(long)]
        mode: u8,
    },
    /// Read the ROM's counter snapshot.
    Diag {
        /// Fail unless the stream-health counters are all zero.
        #[arg(long, default_value_t = false)]
        expect_clean: bool,
    },
    /// M64P HELLO: protocol version, RDRAM size, whether writes are accepted.
    MemHello,
    /// M64P PEEKV: read RDRAM. --addr is an RDRAM physical offset, not a KSEG0 pointer.
    MemPeek {
        #[arg(long, value_parser = parse_u32_maybe_hex)]
        addr: u32,
        #[arg(long)]
        len: u16,
    },
    /// M64P POKEV: write RDRAM (--addr is an RDRAM physical offset). Prefer `mem-round-trip`, which picks a safe address itself.
    MemPoke {
        #[arg(long, value_parser = parse_u32_maybe_hex)]
        addr: u32,
        #[arg(long)]
        hex: String,
    },
    /// M64P PEEKROM: read the cartridge ROM. --addr is a ROM offset (0 is the header).
    MemRomPeek {
        #[arg(long, value_parser = parse_u32_maybe_hex)]
        addr: u32,
        #[arg(long)]
        len: u16,
        /// Fail unless these are the bytes read (hex).
        #[arg(long)]
        expect_hex: Option<String>,
    },
    /// Write, read back and restore the ROM's scratch region (address read from DIAG).
    MemRoundTrip {
        #[arg(long, default_value_t = 64)]
        len: u16,
    },
    Listen {
        #[arg(long, default_value_t = 0.0)]
        duration_secs: f64,
    },
    ControllerPoll {
        #[arg(long, default_value_t = 50)]
        interval_ms: u64,
    },
    /// Run every end-to-end check and report PASS/FAIL for each.
    ///
    /// Needs multi64d running against a cart and multi64_test.z64 booted. The controller is not
    /// needed: the ROM boots into RAW_ECHO and the suite drives it out itself.
    Suite {
        /// Serial port for the direct-serial checks.
        #[arg(long, default_value = multi64_test_connector::suite::DEFAULT_PORT)]
        port: String,
        /// The daemon's HTTP base, for health, link state and release/resume.
        #[arg(long, default_value = multi64_test_connector::suite::DEFAULT_BASE_URL)]
        base: String,
        /// Fail unless the cart reports this ROM version. Without it that check is skipped, not
        /// passed: a version nothing can verify has to stay visible.
        #[arg(long)]
        expect_rom: Option<String>,
        /// Leave the daemon's serial port alone and skip the direct-serial checks.
        #[arg(long, default_value_t = false)]
        skip_serial: bool,
    },
}

/// Accept `0x80000000` as well as a decimal address: RDRAM addresses are always written in hex.
fn parse_u32_maybe_hex(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let r = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => t.parse::<u32>(),
    };
    r.map_err(|e| format!("invalid address {t:?}: {e}"))
}

fn map_command(cmd: Command) -> ConnectorCommand {
    match cmd {
        Command::Ping => ConnectorCommand::Ping,
        Command::Echo { hex, text } => ConnectorCommand::Echo { hex, text },
        Command::Version => ConnectorCommand::Version,
        Command::ReqController => ConnectorCommand::ReqController,
        Command::SessionOpen { hex_challenge } => ConnectorCommand::SessionOpen { hex_challenge },
        Command::SessionClose => ConnectorCommand::SessionClose,
        Command::EepromInfo => ConnectorCommand::EepromInfo,
        Command::EepromRead { offset, len } => ConnectorCommand::EepromRead { offset, len },
        Command::EepromWrite { offset, hex } => ConnectorCommand::EepromWrite { offset, hex },
        Command::SramInfo => ConnectorCommand::SramInfo,
        Command::SramRead { offset, len } => ConnectorCommand::SramRead { offset, len },
        Command::SramWrite { offset, hex } => ConnectorCommand::SramWrite { offset, hex },
        Command::Rumble { port, frames } => ConnectorCommand::Rumble { port, frames },
        Command::DisplayText { text } => ConnectorCommand::DisplayText { text },
        Command::SetMode { mode } => ConnectorCommand::SetMode { mode },
        Command::Diag { expect_clean } => ConnectorCommand::Diag { expect_clean },
        Command::MemHello => ConnectorCommand::MemHello,
        Command::MemPeek { addr, len } => ConnectorCommand::MemPeek { addr, len },
        Command::MemPoke { addr, hex } => ConnectorCommand::MemPoke { addr, hex },
        Command::MemRoundTrip { len } => ConnectorCommand::MemRoundTrip { len },
        Command::MemRomPeek {
            addr,
            len,
            expect_hex,
        } => ConnectorCommand::MemRomPeek {
            addr,
            len,
            expect_hex,
        },
        Command::Listen { .. } | Command::ControllerPoll { .. } | Command::Suite { .. } => {
            unreachable!()
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut log = |line: String| println!("{}", line);

    match args.command {
        Command::Listen { duration_secs } => {
            run_listen(&args.url, duration_secs, None, &mut log).await?;
        }
        Command::Suite {
            port,
            base,
            expect_rom,
            skip_serial,
        } => {
            let opts = SuiteOptions {
                ws_url: args.url.clone(),
                base_url: base,
                port,
                expect_rom,
                skip_serial,
                recv_timeout_secs: args.recv_timeout_secs,
            };
            let mut phase = String::new();
            let mut on = |r: CheckResult| {
                if r.phase != phase {
                    phase = r.phase.clone();
                    println!("\n== {phase} ==");
                }
                match &r.outcome {
                    Outcome::Pass => {
                        println!("PASS  {}{}", r.name, note_suffix(&r.note));
                    }
                    Outcome::Fail { detail } => println!("FAIL  {}  {detail}", r.name),
                    Outcome::Skip { reason } => println!("SKIP  {}  ({reason})", r.name),
                }
            };
            match run_suite(&opts, &mut on).await {
                Err(e) => {
                    // Could not start, as opposed to something failing: exit 2 so a caller can tell
                    // a broken cart from a run that never happened.
                    eprintln!("\nFATAL: {e}");
                    std::process::exit(2);
                }
                Ok(summary) => {
                    println!(
                        "\n== summary ==\n{} passed, {} failed, {} skipped",
                        summary.passed, summary.failed, summary.skipped
                    );
                    if summary.exit_code() != 0 {
                        std::process::exit(summary.exit_code());
                    }
                }
            }
        }
        Command::ControllerPoll { interval_ms } => {
            run_controller_poll(
                &args.url,
                args.recv_timeout_secs,
                interval_ms,
                None,
                &mut log,
            )
            .await?;
        }
        cmd => {
            run_connector_command(
                &args.url,
                args.recv_timeout_secs,
                &map_command(cmd),
                &mut log,
            )
            .await?;
        }
    }

    Ok(())
}
