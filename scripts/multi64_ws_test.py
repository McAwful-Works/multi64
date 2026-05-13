#!/usr/bin/env python3
"""
End-to-end checks against multi64d: HTTP metadata + WebSocket binary/text.

Requires: pip install -r scripts/requirements.txt

Run multi64d first with a supported USB flash cart (reference: SummerCart64) and
multi64_test.z64 in RAW_ECHO mode (default) for binary round-trip, e.g.:
  cargo run -p multi64d --release -- --serial COM3

Full e2e (HTTP + WS hello + ping + binary echo):
  python scripts/multi64_ws_test.py --e2e --assert-echo

HTTP only:
  python scripts/multi64_ws_test.py --http-only
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
from urllib.parse import urlparse


def _need_ws():
    try:
        import websocket  # type: ignore[import-untyped]
    except ImportError:
        print(
            "Missing dependency. Install with:\n  pip install -r scripts/requirements.txt",
            file=sys.stderr,
        )
        raise SystemExit(1)
    return websocket


def ws_url_to_http_base(ws_url: str) -> str:
    u = urlparse(ws_url)
    if u.scheme not in ("ws", "wss"):
        raise SystemExit(f"error: expected ws:// or wss:// URL, got {u.scheme!r}")
    http_scheme = "https" if u.scheme == "wss" else "http"
    if not u.netloc:
        raise SystemExit("error: WebSocket URL must include host (and port if needed)")
    return f"{http_scheme}://{u.netloc}"


def http_check(base: str, timeout: float, quiet: bool) -> None:
    """GET / and /health; validate JSON per docs/spec/daemon-api-v1.md."""
    root = base.rstrip("/") + "/"
    health = base.rstrip("/") + "/health"

    for url, label, check in (
        (root, "GET /", _check_root_json),
        (health, "GET /health", _check_health_json),
    ):
        if not quiet:
            print(f"{label} {url}")
        try:
            req = urllib.request.Request(url, method="GET")
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                body = resp.read()
                if resp.status != 200:
                    raise SystemExit(f"error: {label} HTTP {resp.status}")
        except urllib.error.HTTPError as e:
            raise SystemExit(f"error: {label} HTTP {e.code}: {e.reason}") from e
        except urllib.error.URLError as e:
            raise SystemExit(f"error: {label} failed: {e.reason}") from e

        try:
            data = json.loads(body.decode("utf-8"))
        except json.JSONDecodeError as e:
            raise SystemExit(f"error: {label} not valid JSON: {e}") from e
        check(data, label)
        if not quiet:
            s = json.dumps(data, indent=2)
            print("  ok:", s if len(s) <= 800 else json.dumps(data))


def _check_root_json(data: object, label: str) -> None:
    if not isinstance(data, dict):
        raise SystemExit(f"error: {label} expected JSON object")
    if data.get("service") != "multi64d":
        raise SystemExit(f"error: {label} expected service multi64d, got {data.get('service')!r}")
    if "version" not in data:
        raise SystemExit(f"error: {label} missing version")
    if data.get("websocket_path") != "/ws":
        raise SystemExit(f"error: {label} expected websocket_path /ws, got {data.get('websocket_path')!r}")


def _check_health_json(data: object, label: str) -> None:
    if not isinstance(data, dict):
        raise SystemExit(f"error: {label} expected JSON object")
    if data.get("status") != "ok":
        raise SystemExit(f"error: {label} expected status ok, got {data.get('status')!r}")


def parse_payload(args: argparse.Namespace) -> bytes:
    if args.hex is not None:
        h = args.hex.strip().replace(" ", "")
        if len(h) % 2 != 0:
            raise SystemExit("error: --hex must have an even number of hex digits")
        return bytes.fromhex(h)
    if args.file is not None:
        return args.file.read()
    return args.text.encode("utf-8")


def main() -> None:
    p = argparse.ArgumentParser(
        description="multi64d HTTP + WebSocket e2e test client",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    p.add_argument(
        "--url",
        default="ws://127.0.0.1:38765/ws",
        help="WebSocket URL (multi64d /ws)",
    )
    p.add_argument(
        "--base",
        default=None,
        help="HTTP base URL for GET / and /health (default: derived from --url)",
    )
    p.add_argument(
        "--text",
        default="Multi64 Python e2e",
        help="UTF-8 text for binary frame payload (unless --hex/--file)",
    )
    p.add_argument(
        "--hex",
        metavar="HEX",
        help="send these bytes instead of --text (e.g. deadbeef or 'de ad be ef')",
    )
    p.add_argument(
        "--file",
        type=argparse.FileType("rb"),
        help="read payload bytes from file",
    )
    p.add_argument(
        "--ping",
        action="store_true",
        help='send JSON {"type":"ping"} after server hello (expect pong)',
    )
    p.add_argument(
        "--no-send",
        action="store_true",
        help="only connect, print hello (and optional ping); do not send binary payload",
    )
    p.add_argument(
        "--expect-binary",
        type=int,
        default=1,
        metavar="N",
        help="after sending, wait for up to N binary frames from server",
    )
    p.add_argument(
        "--assert-echo",
        action="store_true",
        help="require first binary response to equal the sent payload (needs test ROM RAW_ECHO)",
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=10.0,
        help="socket / HTTP timeout in seconds",
    )
    p.add_argument("-q", "--quiet", action="store_true", help="less output")
    p.add_argument(
        "--skip-http",
        action="store_true",
        help="do not GET / or /health (WebSocket only)",
    )
    p.add_argument(
        "--http-only",
        action="store_true",
        help="only run HTTP checks and exit",
    )
    p.add_argument(
        "--e2e",
        action="store_true",
        help="shorthand: HTTP checks + ping + binary send + expect 1 binary frame",
    )
    args = p.parse_args()

    if args.e2e:
        args.ping = True
        if not args.no_send:
            args.expect_binary = max(args.expect_binary, 1)

    base = args.base if args.base is not None else ws_url_to_http_base(args.url)

    if not args.skip_http or args.http_only:
        http_check(base, args.timeout, args.quiet)

    if args.http_only:
        print("OK: HTTP checks passed")
        return

    websocket = _need_ws()
    payload = parse_payload(args)

    ws = websocket.create_connection(args.url, timeout=args.timeout)

    try:
        first = ws.recv()
        if not args.quiet:
            print("server (first frame):", _fmt_msg(first))

        if isinstance(first, bytes):
            raise SystemExit("error: expected text hello frame first, got binary")

        try:
            hello = json.loads(first)
        except json.JSONDecodeError as e:
            raise SystemExit(f"error: hello is not JSON: {e}") from e

        if hello.get("type") != "hello":
            raise SystemExit(f"error: expected hello type, got {hello!r}")
        if hello.get("service") != "multi64d":
            raise SystemExit(f"error: expected service multi64d, got {hello.get('service')!r}")
        if "version" not in hello:
            raise SystemExit("error: hello missing version")

        if args.ping:
            ws.send(json.dumps({"type": "ping"}))
            pong = ws.recv()
            if not args.quiet:
                print("pong:", _fmt_msg(pong))
            if isinstance(pong, bytes):
                raise SystemExit("error: expected text pong, got binary")
            try:
                pj = json.loads(pong)
            except json.JSONDecodeError as e:
                raise SystemExit(f"error: pong not JSON: {e}") from e
            if pj.get("type") != "pong":
                raise SystemExit(f"error: expected type pong, got {pj!r}")

        if not args.no_send:
            ws.send(payload, opcode=websocket.ABNF.OPCODE_BINARY)
            if not args.quiet:
                print(f"sent binary: {len(payload)} bytes")

            got = 0
            first_bin: bytes | None = None
            while got < args.expect_binary:
                r = ws.recv()
                if isinstance(r, bytes):
                    got += 1
                    if first_bin is None:
                        first_bin = r
                    print(f"binary[{got}]: len={len(r)}")
                    if args.quiet:
                        print(r.hex())
                    else:
                        print("  hex:", r.hex())
                        try:
                            s = r.decode("utf-8")
                            print("  utf-8:", repr(s))
                        except UnicodeDecodeError:
                            pass
                else:
                    if not args.quiet:
                        print("text (while waiting for binary):", _fmt_msg(r))

            if args.assert_echo and first_bin is not None:
                if first_bin != payload:
                    raise SystemExit(
                        f"error: echo mismatch (sent {len(payload)} bytes, "
                        f"got {len(first_bin)} bytes)\n"
                        f"  sent: {payload.hex()}\n"
                        f"  recv: {first_bin.hex()}"
                    )
                if not args.quiet:
                    print("assert-echo: payload matches")

        if not args.skip_http:
            print("OK: e2e passed (HTTP + WebSocket)")
        else:
            print("OK: WebSocket checks passed (--skip-http)")
    finally:
        ws.close()


def _fmt_msg(m: str | bytes) -> str:
    if isinstance(m, bytes):
        return f"<binary len={len(m)}> hex={m.hex()}"
    return m


if __name__ == "__main__":
    main()
