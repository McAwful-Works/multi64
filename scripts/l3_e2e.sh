#!/usr/bin/env bash
#
# Unattended end-to-end run for the L3 bridge: multi64d + a cart + multi64_test.z64.
#
# Every check runs and reports PASS or FAIL; the run does not stop at the first failure, because
# one broken check hiding the twenty after it is what made the previous script (which this
# replaces) hard to act on. The exit code is 0 only when nothing failed.
#
# What you must do first, and all you must do:
#   1. multi64d running against the cart   (Multi64 starts it, or: cargo run -p multi64d --release -- --serial COM4)
#   2. multi64_test.z64 loaded and booted on the console
# Do NOT touch the controller. The ROM boots into RAW_ECHO and this script drives it out of that
# mode itself (REQ_SET_MODE, test-l3-application-v0.md §10) - that is what makes the run unattended.
#
# Usage:  scripts/l3_e2e.sh [--port COM4] [--url ws://127.0.0.1:38765/ws] [--base http://127.0.0.1:38765]
# Env:    MULTI64_PORT, MULTI64_WS_URL, MULTI64_BASE_URL, MULTI64_EXPECT_ROM, MULTI64_SKIP_SERIAL=1
#
# The serial phase releases the daemon's port so the direct-serial tools can use it. If this script
# is killed in the middle of that, the daemon is left released and Multi64 stays dead until it is
# restarted - so resume runs from an EXIT trap, not just on the happy path.

set -uo pipefail   # deliberately NOT -e: a failing check is data, not a reason to stop.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PORT="${MULTI64_PORT:-COM4}"
WS_URL="${MULTI64_WS_URL:-ws://127.0.0.1:38765/ws}"
BASE_URL="${MULTI64_BASE_URL:-http://127.0.0.1:38765}"
# The ROM this build of the tree expects. A stale ROM on the card makes every later check
# meaningless, so it is checked before anything else is believed.
EXPECT_ROM="${MULTI64_EXPECT_ROM:-$(sed -n 's/.*TEST_ROM_VERSION_STR "\(.*\)".*/\1/p' n64/test-rom/test_proto.h)}"
SKIP_SERIAL="${MULTI64_SKIP_SERIAL:-0}"

while [ $# -gt 0 ]; do
    case "$1" in
        --port) PORT="$2"; shift 2 ;;
        --url) WS_URL="$2"; shift 2 ;;
        --base) BASE_URL="$2"; shift 2 ;;
        --skip-serial) SKIP_SERIAL=1; shift ;;
        -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

PASSED=0
FAILED=0
SKIPPED=0
FAILED_NAMES=""
# Set once the daemon's port has been released, so the trap knows whether to resume.
RELEASED=0

CONNECTOR="target/release/multi64-test-connector"

say() { printf '%s\n' "$*"; }
hr() { printf '\n== %s ==\n' "$*"; }

# --- daemon HTTP -------------------------------------------------------------------
# jq is not installed on every machine that holds this repo, so these pick fields out of the JSON
# with grep. The shapes are fixed by docs/spec/daemon-api-v1.md and covered by multi64d's own tests.

daemon_root() { curl -fsS --max-time 5 "$BASE_URL/" 2>/dev/null; }

daemon_field() { printf '%s' "$1" | grep -o "\"$2\":[^,}]*" | head -1 | sed 's/.*://; s/"//g'; }

# Why a check failed, in the one case the WebSocket cannot tell you: multi64d accepts and discards
# writes while its link is released or faulted, with no error and no close, so a silent cart and a
# dead link look identical from the socket. GET / is the only thing that separates them.
link_note() {
    local root
    root="$(daemon_root)"
    if [ -z "$root" ]; then
        printf ' [daemon did not answer GET / - is multi64d still running?]'
        return
    fi
    if [ "$(daemon_field "$root" serialActive)" != "true" ]; then
        printf ' [serialActive:false - the link was down and the request was dropped before it reached the cart]'
    elif [ "$(daemon_field "$root" serialBusy)" = "true" ]; then
        printf ' [serialBusy:true - the link was held elsewhere; the cart may never have been asked]'
    else
        printf ' [link looks up, so the cart itself did not answer]'
    fi
}

# --- check runner ------------------------------------------------------------------

pass() { PASSED=$((PASSED + 1)); say "PASS  $1"; }
fail() { FAILED=$((FAILED + 1)); FAILED_NAMES="$FAILED_NAMES
  - $1"; say "FAIL  $1${2:+  $2}"; }
skip() { SKIPPED=$((SKIPPED + 1)); say "SKIP  $1${2:+  ($2)}"; }

LAST_OUT=""

# run_check <name> <expect> <connector args...>
#   expect = ok            - the command must succeed
#   expect = fail:PATTERN  - the command must fail AND say PATTERN
#
# A negative check must match on what the cart said, not merely on a non-zero exit. Every way of
# not reaching the cart at all - no daemon, wrong URL, released link - also exits non-zero, so an
# exit-code-only check reports "the cart correctly refused this" when nothing was ever asked. That
# is a false pass, and a negative check that cannot fail is worse than no check.
run_check() {
    local name="$1" expect="$2"; shift 2
    local out rc
    out="$("$CONNECTOR" --url "$WS_URL" "$@" 2>&1)"
    rc=$?
    LAST_OUT="$out"
    case "$expect" in
    fail:*)
        local want="${expect#fail:}"
        if [ $rc -eq 0 ]; then
            fail "$name" "the cart accepted something it should have rejected"
        elif printf '%s' "$out" | grep -q "$want"; then
            pass "$name (refused, as it must be)"
        else
            fail "$name" "failed, but not with \"$want\": $(printf '%s' "$out" | tail -1)$(link_note)"
        fi
        return 0
        ;;
    esac
    if [ $rc -eq 0 ]; then
        pass "$name"
        return 0
    fi
    fail "$name" "$(printf '%s' "$out" | tail -1)$(link_note)"
    return 1
}

# run_tool <name> <command...> - for the direct-serial tools, which are not the connector.
run_tool() {
    local name="$1"; shift
    local out rc
    out="$("$@" 2>&1)"
    rc=$?
    LAST_OUT="$out"
    if [ $rc -eq 0 ]; then
        pass "$name"
    else
        fail "$name" "$(printf '%s' "$out" | tail -1)"
    fi
}

set_mode() { run_check "set mode $(printf '%s' "$2")" ok set-mode --mode "$1"; }

# --- always give the port back -----------------------------------------------------

resume_link() {
    local body
    body="$(curl -fsS --max-time 15 -X POST "$BASE_URL/v1/serial/resume" 2>&1)"
    if printf '%s' "$body" | grep -q '"resumed":true'; then
        RELEASED=0
        return 0
    fi
    printf '%s' "$body"
    return 1
}

on_exit() {
    local rc=$?
    if [ "$RELEASED" = "1" ]; then
        say ""
        say "!! the daemon's serial port is still released - putting it back"
        if resume_link >/dev/null; then
            say "   resumed."
        else
            say "   RESUME FAILED. multi64d is holding no port; restart it (or Multi64) before using the cart."
        fi
    fi
    exit $rc
}
trap on_exit EXIT INT TERM

# --- 0. preflight ------------------------------------------------------------------

hr "preflight"

say "building the connector once (not once per check)"
if ! cargo build -p multi64-test-connector --release >/dev/null 2>&1; then
    say "FATAL: multi64-test-connector does not build; nothing below could be trusted."
    exit 2
fi
[ -x "$CONNECTOR" ] || CONNECTOR="target/release/multi64-test-connector.exe"
if [ ! -x "$CONNECTOR" ]; then
    say "FATAL: built the connector but cannot find its binary at target/release/"
    exit 2
fi

if curl -fsS --max-time 5 "$BASE_URL/health" 2>/dev/null | grep -q '"status":"ok"'; then
    pass "daemon answers /health"
else
    say "FATAL: no multi64d at $BASE_URL. Start Multi64, or: cargo run -p multi64d --release -- --serial $PORT"
    exit 2
fi

ROOT_JSON="$(daemon_root)"
DAEMON_SERIAL="$(daemon_field "$ROOT_JSON" serial)"
DAEMON_CART="$(daemon_field "$ROOT_JSON" cart)"
say "daemon: serial=$DAEMON_SERIAL cart=$DAEMON_CART"

if [ "$(daemon_field "$ROOT_JSON" serialActive)" = "true" ]; then
    pass "daemon holds its serial port"
else
    # Not fatal on its own - the daemon retries once a second - but every cart check below will
    # fail, and they would each blame the cart. Say so once, here.
    fail "daemon holds its serial port" "serialActive:false; the cart checks below cannot pass"
fi

# --- 1. the cart is alive, and it is the ROM we think it is ------------------------

hr "cart liveness and identity"

# Also the proof that REQ_SET_MODE works from RAW_ECHO: the ROM boots there, nobody has touched
# the controller, and this is the first thing sent.
set_mode 1 "M64T_PROTO (from whatever it booted into)"

run_check "cart answers PING" ok ping

if run_check "cart reports its ROM version" ok version; then
    if printf '%s' "$LAST_OUT" | grep -q "$EXPECT_ROM"; then
        pass "ROM on the cart is $EXPECT_ROM"
    else
        # The most valuable check here. Everything below tests whatever ROM is actually running,
        # so a stale card silently turns the rest of this run into fiction.
        fail "ROM on the cart is $EXPECT_ROM" \
            "got $(printf '%s' "$LAST_OUT" | sed -n 's/.*VERSION "\(.*\)".*/\1/p' | tail -1); rebuild and re-upload n64/test-rom/multi64_test.z64"
    fi
fi

run_check "cart reports its counters" ok diag
BASE_DIAG="$LAST_OUT"
printf '%s\n' "$BASE_DIAG" | sed -n 's/^DIAG /  /p'

# --- 2. the M64T surface -----------------------------------------------------------

hr "M64T request/response"

run_check "echo returns what was sent" ok echo --text "multi64 l3 e2e"
run_check "echo of 4 KiB crosses USB chunks" ok echo --hex "$(printf '5a%.0s' $(seq 1 4096))"
run_check "controller snapshot" ok req-controller

run_check "session opens" ok session-open
run_check "eeprom reports its geometry" ok eeprom-info
run_check "eeprom reads" ok eeprom-read --offset 0 --len 16
run_check "eeprom writes inside a session" ok eeprom-write --offset 0 --hex 0102030405060708
run_check "sram reports its geometry" ok sram-info
run_check "session closes" ok session-close

# Deliberately not a PASS/FAIL: the effect is on the console and the desk, and the host cannot see
# either. Reporting them as passes would count two checks that only prove the ROM sent an ack.
say ""
say "  (actuated, not verified - the host cannot observe these)"
"$CONNECTOR" --url "$WS_URL" display-text --text "l3 e2e running" >/dev/null 2>&1 \
    && say "  display-text: acked" || say "  display-text: no ack"
"$CONNECTOR" --url "$WS_URL" rumble --port 0 --frames 30 >/dev/null 2>&1 \
    && say "  rumble: acked" || say "  rumble: no ack"

# --- 3. M64P, the RDRAM peek/poke profile -----------------------------------------

hr "M64P peek/poke"

run_check "M64P says hello" ok mem-hello
run_check "write, read back and restore RDRAM" ok mem-round-trip --len 64
# The error path is worth as much as the happy one: an agent that answers ERR for an address
# outside RDRAM is an agent that will not scribble over something else when a host asks it to.
# M64P addresses are RDRAM physical offsets, so 0 is the *start* of RDRAM and perfectly valid -
# this has to be an offset past the end of any N64's memory (8 MiB expanded).
run_check "a read outside RDRAM is refused" "fail:PEEKV rejected" mem-peek --addr 0x7F000000 --len 16

# --- 4. BENCH: the cart talking without being asked --------------------------------

hr "BENCH (unsolicited cart traffic)"

set_mode 2 "BENCH"
say "listening 3s for BENCH_TICK"
TICKS="$("$CONNECTOR" --url "$WS_URL" listen --duration-secs 3 2>&1 | grep -c 'BENCH_TICK')"
if [ "${TICKS:-0}" -ge 2 ]; then
    pass "cart sends BENCH_TICK unprompted ($TICKS in 3s)"
else
    fail "cart sends BENCH_TICK unprompted" "saw $TICKS in 3s, expected at least 2$(link_note)"
fi
set_mode 1 "M64T_PROTO"

# --- 5. the direct-serial path -----------------------------------------------------
# These tools open the COM port themselves and need the ROM in RAW_ECHO, which is why nothing has
# ever chained them with the WebSocket checks above: the two need mutually exclusive cart states,
# and until REQ_SET_MODE existed only a person with a controller could switch between them.

hr "direct serial (RAW_ECHO)"

if [ "$SKIP_SERIAL" = "1" ]; then
    skip "direct-serial checks" "--skip-serial"
else
    set_mode 0 "RAW_ECHO"

    if curl -fsS --max-time 10 -X POST "$BASE_URL/v1/serial/release" 2>/dev/null | grep -q '"released":true'; then
        RELEASED=1
        pass "daemon released $PORT"

        run_tool "serial echo round trip" \
            cargo run -q -p sc64-echo-test --release -- --port "$PORT"
        run_tool "L3 framing over serial, including an 8 KiB frame" \
            cargo run -q -p sc64-l3-framing-e2e --release -- --port "$PORT" --large

        # Capturing the output puts resume_link in a subshell, so clear the flag here in the
        # parent: otherwise the EXIT trap below announces the port is still released, resumes an
        # already-resumed link, and ends a clean run with an alarming message.
        if RESUME_ERR="$(resume_link)"; then
            RELEASED=0
            pass "daemon resumed $PORT"
        else
            fail "daemon resumed $PORT" "$RESUME_ERR"
        fi
        # Resume returning 200 means it reopened the handle, not that the cart is back. Prove it.
        if [ "$(daemon_field "$(daemon_root)" serialActive)" = "true" ]; then
            pass "link is back up after resume"
        else
            fail "link is back up after resume" "serialActive:false; restart multi64d"
        fi
        # Back to M64T_PROTO *before* pinging: the cart is still in RAW_ECHO from the serial
        # checks, and RAW_ECHO echoes a PING rather than answering it, so pinging first fails for
        # a reason that has nothing to do with whether the link came back.
        set_mode 1 "M64T_PROTO"
        run_check "cart still answers through the daemon" ok ping
    else
        fail "daemon released $PORT" "release refused; skipping the direct-serial checks"
        skip "serial echo round trip" "port not released"
        skip "L3 framing over serial" "port not released"
        set_mode 1 "M64T_PROTO"
    fi
fi

# --- 6. was the stream intact the whole way through? ------------------------------

hr "stream health"

# The counters reset on the last mode change, so this covers everything since then. A reply to
# every request above proves each round trip; only these prove nothing desynchronised underneath.
run_check "no overflow, resync or bad headers since the last mode change" ok diag --expect-clean
printf '%s\n' "$LAST_OUT" | sed -n 's/^DIAG /  /p'

# --- summary -----------------------------------------------------------------------

hr "summary"
say "$PASSED passed, $FAILED failed, $SKIPPED skipped"
if [ "$FAILED" -ne 0 ]; then
    say "failed:$FAILED_NAMES"
    exit 1
fi
exit 0
