//! Which flash cart a patched ROM's agent talks to.
//!
//! The agent drives one cart: `n64/agent` builds with one driver, and the drivers do not detect
//! each other. So every profile carries a build per cart (`crates/ap64-core/agent/build.sh`), and
//! a seed is patched with the one for the cart it will run on. Only the SummerCart64's driver has
//! run on hardware; the EverDrives' never have.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cart {
    Sc64,
    Ed64,
    Ed64pro,
}

impl Cart {
    /// Every cart, the one a profile's own directory holds first.
    pub const ALL: [Cart; 3] = [Cart::Sc64, Cart::Ed64, Cart::Ed64pro];

    /// The name `multi64d --cart` and the agent's `CART=` use, and the subdirectory of a profile
    /// that holds this cart's build (the SummerCart64's is the profile directory itself).
    pub fn id(self) -> &'static str {
        match self {
            Cart::Sc64 => "sc64",
            Cart::Ed64 => "ed64",
            Cart::Ed64pro => "ed64pro",
        }
    }

    /// Shown to the user.
    pub fn name(self) -> &'static str {
        match self {
            Cart::Sc64 => "SummerCart64",
            Cart::Ed64 => "EverDrive-64 X7",
            Cart::Ed64pro => "EverDrive-64 PRO",
        }
    }

    /// Whether its agent has run on a cart. Only the SummerCart64's has.
    pub fn tested(self) -> bool {
        self == Cart::Sc64
    }

    pub fn parse(id: &str) -> Option<Cart> {
        Cart::ALL.into_iter().find(|c| c.id() == id)
    }
}

impl std::fmt::Display for Cart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
