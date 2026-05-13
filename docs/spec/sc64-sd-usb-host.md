# SummerCart64 — host SD access over USB (`SD_CARD_OP`)

**Spec-Revision:** 1  

Normative for **Multi64** host tooling that talks to the SC64 SD from the PC (`multi64-sc64-sd`, Xfer64 **USB serial** mode). Vendor overview: SummerCart64 [`docs/03_usb_interface.md`](https://github.com/Polprzewodnikowy/SummerCart64/blob/main/docs/03_usb_interface.md) (`CMD` id **`i`** — **SD_CARD_OP**).

---

## 1. `arg1` operation codes (wire)

The `CMD` packet carries **`arg0`** and **`arg1`** as **big-endian `uint32`** (see vendor USB doc).

The **reference implementation** is **`sc64deployer`** (`sw/deployer/src/sc64/types.rs`), `impl From<SdCardOp> for [u32; 2]`:

| Operation   | `[arg0, arg1]` on the wire |
|------------|----------------------------|
| Deinit     | `[0, 0]`                   |
| Init       | `[0, 1]`                   |
| Get status | `[0, 2]`                   |
| Get info   | `[address, 3]`             |
| Byte swap on  | `[0, 4]`               |
| Byte swap off | `[0, 5]`               |

**Important:** The markdown table in the upstream file **`docs/03_usb_interface.md`** (“Available SD card operations”) lists **Init** as operation **`0`** and **Deinit** as **`1`**. That **does not** match the **`arg1`** values **`sc64deployer`** sends. Host code **MUST** use the **`[arg0, arg1]`** mapping above (same as deployer), not the markdown row index as `arg1`.

---

## 2. Error payload

On **`ERR`**, the response data for **`SD_CARD_OP`** is described in the vendor doc: first `uint32` is **`sd_error_t`** (`sw/controller/src/sd.h`), second is SD status. **`SD_ERROR_LOCKED` (30)** means the other side (typically the N64) still holds the SD lock; power off the console if needed.

---

## 3. References

- SummerCart64 `docs/03_usb_interface.md` — packet layout, `SD_READ` / `MEMORY_READ`.
- `sw/deployer/src/sc64/mod.rs` — `command_sd_card_operation`, `init_sd_card`, `deinit_sd_card`.
- `sw/deployer/src/sc64/types.rs` — `SdCardOp` → `[arg0, arg1]`.
