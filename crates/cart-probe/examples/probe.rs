//! Look for a cart the way Multi64's and Xfer64's **Auto** settings do.
//!
//! ```sh
//! cargo run -p multi64-cart-probe --example probe              # list ports, judge USB IDs; sends nothing
//! cargo run -p multi64-cart-probe --example probe -- COM4      # probe only the ports named
//! ```
//!
//! Probing sends cart test commands to each named port, and cannot open a port another process
//! holds: release `multi64d` first (`POST /v1/serial/release`).

use multi64_cart_probe::{probe_port, usb_is_sc64};
use serialport::SerialPortType;
use std::time::Instant;

fn main() {
    let ports: Vec<String> = std::env::args().skip(1).collect();
    if ports.is_empty() {
        for p in serialport::available_ports().unwrap_or_default() {
            let verdict = match &p.port_type {
                SerialPortType::UsbPort(usb) if usb_is_sc64(usb) => {
                    "SummerCart64, by its USB IDs".to_string()
                }
                SerialPortType::UsbPort(usb) => format!(
                    "USB {:04x}:{:04x}, not recognized by its USB IDs",
                    usb.vid, usb.pid
                ),
                _ => "not a USB device".to_string(),
            };
            println!("{}: {verdict}", p.port_name);
        }
        println!("Name ports to probe them; each one receives cart test commands.");
        return;
    }
    for port in ports {
        let started = Instant::now();
        match probe_port(&port) {
            Some(cart) => println!("{port}: {} ({:?})", cart.as_str(), started.elapsed()),
            None => println!("{port}: no cart answered ({:?})", started.elapsed()),
        }
    }
}
