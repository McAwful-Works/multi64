#!/usr/bin/env bash
# Hardware E2E for multi64-test-connector (requires multi64d + USB cart + test ROM in M64T_PROTO or BENCH).
# Reference transport: multi64d with SummerCart64 L2. See docs/connectors/test-rom.md.
# Usage (from repo root): ./scripts/test_rom_connector_e2e.sh
# Env: MULTI64_WS_URL (default ws://127.0.0.1:38765/ws), MULTI64_RECV_TIMEOUT_SECS (optional)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

URL="${MULTI64_WS_URL:-ws://127.0.0.1:38765/ws}"
CARGO=(cargo run -p multi64-test-connector --release -- --url "$URL")

if [[ -n "${MULTI64_RECV_TIMEOUT_SECS:-}" ]]; then
  CARGO+=(--recv-timeout-secs "${MULTI64_RECV_TIMEOUT_SECS}")
fi

run() {
  echo "==> multi64-test-connector $*"
  "${CARGO[@]}" "$@"
}

run ping
run version
run echo --text "Multi64 e2e"
run req-controller
run rumble --port 0 --frames 60
run display-text --text "Multi64 e2e"
run session-open
run eeprom-info
run eeprom-read --offset 0 --len 16
run eeprom-write --offset 0 --hex 0102030405060708
run sram-info
run session-close
run listen --duration-secs 2

echo "OK: test ROM connector E2E script finished."
