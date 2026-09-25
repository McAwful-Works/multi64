//! Banjo-Tooie: how its client is answered.
//!
//! Banjo-Tooie Client (`BTClient.py`) keeps the game logic in its own Python
//! (`client/game.py`, `client/state.py`) and reaches the game through the same EmuLoader
//! library DK64 Client uses ([`crate::dk64`]): v0.1.4, byte for byte, unpacked in the apworld
//! rather than vendored in a zip. With no emulator running it falls back to RetroArch's
//! Network Commands, which is what AP64 answers ([`crate::retroarch`]). The client's other
//! transport, a socket on port 21221 for upstream's EverDrive program, idles once EmuLoader is
//! attached. Issue #319.
//!
//! # No client fix
//!
//! This EmuLoader has DK64's gap too: its RetroArch backend has no `read_bytestring` or
//! `write_bytestring`. Nothing on Banjo-Tooie Client's path calls either. It reads and writes
//! only `u8`, `u16` and `u32`, so it reaches AP64 as released. Its signature check
//! (`validate_bt_signature`) is applied to emulator processes only, never to the RetroArch
//! fallback.
//!
//! # No bitwise ranges
//!
//! The client reads the game's flag bitmaps (the randomizer's `n64_saves_*`) and never writes
//! them. Every write it makes is a whole byte, `u16` or `u32`: settings, item and trap counts,
//! the exit map, the message buffer, and the death link, tag link and text-queue counters.
//! [`crate::retroarch`] already writes only the bytes the client changed, so no range has to be
//! merged a bit at a time.
//!
//! # The settings
//!
//! By the client's own account, the ROM refuses to boot until the randomizer's settings are in
//! RAM. The client writes them (`write_slot_settings`) whenever the seed number in RAM differs
//! from its slot's, and they go out in the order it made them, like any other writes. This is
//! the work the forked connector script used to do in `process_slot()`.

use crate::retroarch::Options;

/// RetroArch's own port, which is where EmuLoader looks.
pub const OPTIONS: Options = Options {
    port: 55355,
    bitwise: &[],
};
