"""Sit between an Archipelago client and AP64, time every round trip, and record who hangs up.

Neither end's log answers the two questions that matter when a session keeps dropping:
which side closed the socket, and how long the cart actually took. The client logs nothing
at all when it gives up, and AP64 only sees its own side. This sits in the middle and
records both, which turns "it keeps disconnecting" into a number.

It was written after four wrong diagnoses of the same symptom -- Expansion Pak
framebuffers, agent latency, server-side eviction -- each argued from a plausible mechanism
rather than from evidence about the actual failure. Two minutes of this settled it: the
CLIENT was hanging up, every request had been answered (16,437 sent, 16,437 returned), and
the slow round trips sat on exact multiples of the cart's REPLY_TIMEOUT.

WHAT THE SHAPE TELLS YOU

Healthy traffic is ~10 ms with a p90 under 100 ms. A miss costs a flat REPLY_TIMEOUT, so
the tail is QUANTIZED: values cluster at 1x, 2x and 3x that constant with nothing in
between. If you see a gap between ~250 ms and the first cluster, that is this, and the
cluster's position names the constant to look at. Gradual spread instead would mean
something genuinely slow rather than something missed.

Every Archipelago client enforces its own per-request deadline and reconnects silently when
it fires -- 5 s for the BizHawk Client (worlds/_bizhawk/__init__.py, `_send_message`). Any
round trip past DEADLINE is a dropout the player sees, so those are called out as they
happen.

HOW IT ATTACHES

AP64 binds the first free port in its script's range, so this takes the first one and
forwards to the second. Start it BEFORE AP64:

    python link-tap.py                  # generic/BizHawk Client games: 43055 -> 43056
    python link-tap.py --listen 28921 --upstream 28922     # Ocarina of Time

then start AP64 (it lands on the upstream port) and the client (it finds this one). Writes
link-tap.txt next to itself.
"""

import argparse
import os
import socket
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))

_lock = threading.Lock()
_conn_no = 0
_lat = []       # every round trip, seconds
_worst = []     # (seconds, request prefix), trimmed
_last_report = time.time()
_args = None


def log(msg):
    line = f"{time.strftime('%H:%M:%S')}  {msg}"
    with _lock:
        print(line, flush=True)
        try:
            with open(_args.out, "a", encoding="utf-8") as f:
                f.write(line + "\n")
        except OSError:
            pass


def report():
    """Histogram and tail. The mean hides everything that matters here."""
    global _last_report
    with _lock:
        n = len(_lat)
        if n == 0:
            return
        s = sorted(_lat)
        worst = sorted(_worst, reverse=True)[:3]
        _last_report = time.time()

    def pct(p):
        return s[min(n - 1, int(n * p))]

    # Sorted and de-duplicated, because --deadline can fall anywhere among the fixed
    # edges: a deadline under 2 s used to be appended after them, so the buckets came out
    # in the wrong order and counted the wrong ranges.
    fixed = [0.05, 0.1, 0.25, 0.5, 1.0, 2.0]
    cuts = sorted({e for e in fixed if e < _args.deadline} | {_args.deadline})
    counts, prev = [], 0.0
    for hi in cuts + [float("inf")]:
        if hi == float("inf"):
            name = f">={_args.deadline:g}s DROPOUT"
        elif hi < 1.0:
            name = f"<{hi * 1000:.0f}ms"
        else:
            name = f"<{hi:g}s"
        counts.append(f"{name} {sum(1 for x in s if prev <= x < hi)}")
        prev = hi

    log(f"LATENCY n={n}  p50={pct(0.5) * 1000:.0f}ms p90={pct(0.9) * 1000:.0f}ms "
        f"p99={pct(0.99) * 1000:.0f}ms max={s[-1] * 1000:.0f}ms")
    log("  buckets: " + "  ".join(counts))
    for sec, req in worst:
        log(f"  worst {sec * 1000:.0f}ms <- {req[:90]}")


def from_client(src, dst, st):
    """Client requests. Each line starts a round trip."""
    buf = b""
    try:
        while True:
            b = src.recv(65536)
            if not b:
                st["closed_by"] = st["closed_by"] or "client"
                break
            buf += b
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                st["req"] += 1
                with st["qlock"]:
                    st["pending"].append((time.time(), line[:100].decode("utf-8", "replace")))
            dst.sendall(b)
    except OSError as e:
        st["closed_by"] = st["closed_by"] or "client(ERR)"
        st["error"] = f"client: {e}"
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def from_ap64(src, dst, st):
    """AP64 responses. Each line completes the oldest outstanding request."""
    buf = b""
    try:
        while True:
            b = src.recv(65536)
            if not b:
                st["closed_by"] = st["closed_by"] or "ap64"
                break
            buf += b
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                st["resp"] += 1
                now = time.time()
                with st["qlock"]:
                    t0, req = st["pending"].pop(0) if st["pending"] else (now, "(unmatched)")
                dt = now - t0
                with _lock:
                    _lat.append(dt)
                    if dt >= 0.5:
                        _worst.append((dt, req))
                        del _worst[:-50]
                if dt >= _args.deadline:
                    log(f"!! {dt * 1000:.0f}ms round trip -- past the client's "
                        f"{_args.deadline:g}s deadline, this is a dropout <- {req[:80]}")
            dst.sendall(b)
    except ConnectionResetError:
        # AP64 ends a client with a reset rather than a close, deliberately.
        st["closed_by"] = st["closed_by"] or "ap64(RESET)"
        st["error"] = "ap64: connection reset"
    except OSError as e:
        st["closed_by"] = st["closed_by"] or "ap64(ERR)"
        st["error"] = f"ap64: {e}"
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def serve(client, addr):
    global _conn_no
    with _lock:
        _conn_no += 1
        n = _conn_no
    t0 = time.time()
    try:
        up = socket.create_connection(("127.0.0.1", _args.upstream), timeout=10)
    except OSError as e:
        log(f"conn#{n} could not reach AP64 on {_args.upstream}: {e}  "
            f"(start this tap BEFORE AP64 so AP64 lands on {_args.upstream})")
        client.close()
        return
    for s in (client, up):
        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    log(f"conn#{n} OPEN from {addr[0]}:{addr[1]}")

    st = {"closed_by": None, "req": 0, "resp": 0, "pending": [], "qlock": threading.Lock()}
    a = threading.Thread(target=from_client, args=(client, up, st), daemon=True)
    b = threading.Thread(target=from_ap64, args=(up, client, st), daemon=True)
    a.start()
    b.start()
    a.join()
    b.join()
    client.close()
    up.close()

    # req == resp means AP64 answered everything and the client left anyway; req > resp
    # means a reply was still outstanding, which is what killing AP64 looks like too.
    log(f"conn#{n} CLOSED after {time.time() - t0:.1f}s -- FIRST TO HANG UP: {st['closed_by']}"
        f"  | req {st['req']} resp {st['resp']}"
        + (f"  | {st['error']}" if st.get("error") else ""))
    report()


def ticker():
    while True:
        time.sleep(5)
        if time.time() - _last_report >= _args.every:
            report()


def main():
    global _args
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--listen", type=int, default=43055,
                   help="port the client finds; AP64's first (default: 43055, generic)")
    p.add_argument("--upstream", type=int, default=43056,
                   help="port AP64 lands on once this holds --listen (default: 43056)")
    p.add_argument("--deadline", type=float, default=5.0,
                   help="the client's per-request deadline; past it is a dropout (default: 5)")
    p.add_argument("--every", type=float, default=60.0,
                   help="seconds between distribution reports (default: 60)")
    p.add_argument("--out", default=os.path.join(HERE, "link-tap.txt"),
                   help="log file (default: link-tap.txt next to this script)")
    _args = p.parse_args()

    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", _args.listen))
    s.listen(8)
    log(f"tap on {_args.listen} -> AP64 {_args.upstream}; a round trip past "
        f"{_args.deadline:g}s is what the client calls a dropout")
    log("start AP64 AFTER this, so it binds the upstream port and the client finds the tap")
    threading.Thread(target=ticker, daemon=True).start()
    while True:
        c, a = s.accept()
        threading.Thread(target=serve, args=(c, a), daemon=True).start()


if __name__ == "__main__":
    main()
