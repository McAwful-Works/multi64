//! Reference daemon binary — see `multi64d` library and `docs/spec/daemon-api-v1.md`.

use clap::Parser;
use multi64d::config::{load_config_file, merge, resolve_config_path, FileConfig};
use multi64d::{build_app, cart_reader_loop, open_pipe, AppState, LinkState, SerialConfig};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Plain text when stderr is piped (e.g. multi64 GUI captures multi64d logs); colors in a real terminal.
/// `tracing_subscriber::fmt` writes to **stderr** by default — check that stream, not stdout.
/// See <https://no-color.org/>.
fn tracing_use_ansi() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}

#[derive(Parser, Debug)]
#[command(
    name = "multi64d",
    about = "multi64 WebSocket bridge (L3 stream; SummerCart64 serial backend)"
)]
struct Args {
    /// TOML config file (env: `MULTI64D_CONFIG`). If omitted, tries `./multi64d.toml` then OS config dir.
    #[arg(long, env = "MULTI64D_CONFIG")]
    config: Option<PathBuf>,

    /// USB serial device (e.g. COM3, /dev/ttyACM0). Env: `MULTI64D_SERIAL`.
    #[arg(short, long, env = "MULTI64D_SERIAL")]
    serial: Option<String>,

    /// Env: `MULTI64D_BAUD`
    #[arg(long, env = "MULTI64D_BAUD")]
    baud: Option<u32>,

    /// TCP listen address for HTTP + WebSocket. Env: `MULTI64D_LISTEN`
    #[arg(long, env = "MULTI64D_LISTEN")]
    listen: Option<String>,

    /// Clear host serial buffers after opening the port. Env: `MULTI64D_CLEAR_SERIAL` (true/false).
    /// `Option` so an explicit `false` can override `clear_serial = true` in a config file; a bare
    /// `--clear-serial` still means `true`.
    #[arg(
        long,
        env = "MULTI64D_CLEAR_SERIAL",
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL"
    )]
    clear_serial: Option<bool>,

    /// Browser origin allowed to call the daemon (repeatable). Native clients send no `Origin`
    /// header and are always allowed; a browser always sends one and is rejected unless listed
    /// here. Env: `MULTI64D_ALLOW_ORIGIN` (comma-separated).
    #[arg(
        long,
        env = "MULTI64D_ALLOW_ORIGIN",
        value_delimiter = ',',
        value_name = "ORIGIN"
    )]
    allow_origin: Vec<String>,

    /// Skip logging available serial ports at startup (default is to log them at info level).
    #[arg(long = "no-print-ports", default_value_t = false, action = clap::ArgAction::SetTrue)]
    no_print_ports: bool,

    /// List serial port names to stdout and exit (for scripts).
    #[arg(long, default_value_t = false)]
    list_ports: bool,

    /// Log every non-empty serial read from the cart (`trace!` in `multi64-sc64-l2`).
    /// Also set env `MULTI64D_SERIAL_TRACE=1` (see `build_env_filter`).
    #[arg(long, default_value_t = false)]
    serial_trace: bool,
}

fn env_multi64d_serial_trace() -> bool {
    std::env::var("MULTI64D_SERIAL_TRACE")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// `tracing_subscriber::fmt` defaults to `info` when `RUST_LOG` is unset; our serial chunks use
/// `trace!`, so they never appear unless `RUST_LOG` includes `multi64_sc64_l2=trace`.
/// `--serial-trace` / `MULTI64D_SERIAL_TRACE=1` prepends that directive (and merges with `RUST_LOG`
/// if set).
fn build_env_filter(serial_trace_cli: bool) -> tracing_subscriber::EnvFilter {
    let want_serial_trace = serial_trace_cli || env_multi64d_serial_trace();
    if want_serial_trace {
        if let Ok(u) = std::env::var("RUST_LOG") {
            let u = u.trim();
            if !u.is_empty() {
                let combined = format!("multi64_sc64_l2=trace,{u}");
                return combined.parse().unwrap_or_else(|_| {
                    "multi64_sc64_l2=trace,tower_http=error,info"
                        .parse()
                        .expect("fallback filter")
                });
            }
        }
        return "multi64_sc64_l2=trace,tower_http=error,info"
            .parse()
            .expect("embedded filter");
    }
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".parse().expect("info filter"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_ansi(tracing_use_ansi())
        .with_env_filter(build_env_filter(args.serial_trace))
        .init();

    if args.serial_trace || env_multi64d_serial_trace() {
        tracing::info!(
            target: "multi64d",
            "serial trace on (stderr): non-empty reads from cart emit TRACE on target multi64_sc64_l2"
        );
    }

    if args.list_ports {
        list_ports_stdout()?;
        return Ok(());
    }

    let (file_cfg, loaded_path) = load_merged_file_config(&args)?;
    let resolved = merge(
        args.serial.clone(),
        args.baud,
        args.listen.clone(),
        args.clear_serial,
        args.allow_origin.clone(),
        file_cfg,
        loaded_path,
    )?;

    if let Some(ref p) = resolved.config_path {
        tracing::info!(path = %p.display(), "loaded config file");
    }
    // Before opening the port, so a failed open still shows which settings were in effect.
    tracing::debug!(
        serial = %resolved.serial,
        baud = resolved.baud,
        listen = %resolved.listen,
        clear_serial = resolved.clear_serial,
        allow_origin = ?resolved.allow_origin,
        "resolved configuration"
    );
    if !args.no_print_ports {
        log_serial_ports_tracing()?;
    }

    let serial_cfg = SerialConfig {
        path: resolved.serial.clone(),
        baud: resolved.baud,
        clear_serial: resolved.clear_serial,
    };
    let pipe = open_pipe(&serial_cfg)?;

    let (from_cart, _) = broadcast::channel::<Vec<u8>>(256);

    let state = Arc::new(AppState::new(
        serial_cfg,
        LinkState::Active(pipe),
        from_cart.clone(),
        resolved.allow_origin.clone(),
    ));

    tokio::spawn(cart_reader_loop(state.clone()));

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(&resolved.listen).await?;
    tracing::info!(
        listen = %resolved.listen,
        serial = %resolved.serial,
        baud = resolved.baud,
        clear_serial = resolved.clear_serial,
        allow_origin = ?resolved.allow_origin,
        "multi64d started (cart reader runs always; WebSocket clients receive broadcast from cart)"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

fn load_merged_file_config(args: &Args) -> anyhow::Result<(FileConfig, Option<PathBuf>)> {
    if let Some(ref p) = args.config {
        if !p.exists() {
            anyhow::bail!("config file not found: {}", p.display());
        }
        let cfg = load_config_file(p)?;
        return Ok((cfg, Some(p.clone())));
    }
    if let Some(p) = resolve_config_path(None) {
        let cfg = load_config_file(&p)?;
        return Ok((cfg, Some(p)));
    }
    Ok((FileConfig::default(), None))
}

fn list_ports_stdout() -> anyhow::Result<()> {
    for p in serialport::available_ports()? {
        println!("{}", p.port_name);
    }
    Ok(())
}

fn log_serial_ports_tracing() -> anyhow::Result<()> {
    let ports = serialport::available_ports()?;
    if ports.is_empty() {
        tracing::warn!("no serial ports found on this host");
        return Ok(());
    }
    for p in ports {
        tracing::info!(name = %p.port_name, kind = ?p.port_type, "serial port");
    }
    Ok(())
}
