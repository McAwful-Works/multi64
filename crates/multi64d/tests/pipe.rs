//! `open_pipe` dispatch, checked without a cart: every backend must turn an unopenable device into
//! an error the daemon can log and retry, never a panic. Proves only the wiring — no pipe has a
//! device on the other end here, and the EverDrive ones have never had one at all.

use multi64d::{open_pipe, CartKind, SerialConfig};

/// A device name no host will have, on Windows or Unix.
const NO_SUCH_PORT: &str = "multi64d-test-no-such-serial-port";

fn cfg(cart: CartKind) -> SerialConfig {
    SerialConfig {
        path: NO_SUCH_PORT.into(),
        baud: 115200,
        clear_serial: true,
        cart,
    }
}

#[test]
fn sc64_open_of_missing_port_is_an_error() {
    assert!(open_pipe(&cfg(CartKind::Sc64)).is_err());
}

#[test]
fn ed64_open_of_missing_port_is_an_error() {
    assert!(open_pipe(&cfg(CartKind::Ed64)).is_err());
}

#[test]
fn ed64pro_open_of_missing_port_is_an_error() {
    assert!(open_pipe(&cfg(CartKind::Ed64Pro)).is_err());
}

#[test]
fn cart_kind_names_match_the_cli_and_config_spelling() {
    assert_eq!(CartKind::default(), CartKind::Sc64);
    assert_eq!(CartKind::Sc64.to_string(), "sc64");
    assert_eq!(CartKind::Ed64.to_string(), "ed64");
    assert_eq!(CartKind::Ed64Pro.to_string(), "ed64pro");
    // clap derives kebab-case (`ed64-pro`) unless told otherwise; the CLI must match the config.
    use clap::ValueEnum;
    assert_eq!(
        CartKind::Ed64Pro.to_possible_value().unwrap().get_name(),
        "ed64pro"
    );
}
