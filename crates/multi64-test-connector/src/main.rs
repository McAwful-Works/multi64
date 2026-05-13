//! CLI for **`multi64_test.z64`** over **`multi64d`** WebSocket — see [`docs/connectors/test-rom.md`](../../docs/connectors/test-rom.md).

use anyhow::Result;
use clap::{Parser, Subcommand};
use multi64_test_connector::{
    run_connector_command, run_controller_poll, run_listen, ConnectorCommand,
};

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
    Listen {
        #[arg(long, default_value_t = 0.0)]
        duration_secs: f64,
    },
    ControllerPoll {
        #[arg(long, default_value_t = 50)]
        interval_ms: u64,
    },
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
        Command::Listen { .. } | Command::ControllerPoll { .. } => unreachable!(),
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
