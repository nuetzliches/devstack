"""Promotion comparison for ADR-0010: what a client sees while a service is
replaced. Pollers request URLs every 50 ms, an SSE client and a WebSocket
client stay connected (and reconnect at once); between a baseline and a tail
the trigger command runs (a restart in place, or a proxy promote).

  python bench.py --name A --poll http://127.0.0.1:4201/health \
      --poll https://localhost:4200/api/auth/me --sse http://127.0.0.1:5080/events \
      --ws ws://127.0.0.1:5080/ws --trigger "devstack restart api" --tail 10

A poll counts as served when the answer is below 500 (a 401 is the service
answering). Results are printed as one JSON object.
"""

import argparse
import base64
import http.client
import json
import os
import socket
import ssl
import subprocess
import threading
import time
import urllib.parse

STOP = threading.Event()
CTX = ssl.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = ssl.CERT_NONE


def now():
    return time.monotonic()


def conn_for(url, timeout):
    u = urllib.parse.urlsplit(url)
    port = u.port or (443 if u.scheme in ("https", "wss") else 80)
    if u.scheme in ("https", "wss"):
        return http.client.HTTPSConnection(u.hostname, port, timeout=timeout, context=CTX), u
    return http.client.HTTPConnection(u.hostname, port, timeout=timeout), u


def poller(url, out):
    while not STOP.is_set():
        t = now()
        try:
            c, u = conn_for(url, 5)
            c.request("GET", u.path or "/")
            r = c.getresponse()
            r.read()
            out.append((t, r.status < 500, r.status, now() - t, r.getheader("x-devstack-upstream")))
            c.close()
        except Exception as e:  # noqa: BLE001
            out.append((t, False, type(e).__name__, now() - t, None))
        time.sleep(max(0, 0.05 - (now() - t)))


def sse(url, events, drops):
    while not STOP.is_set():
        try:
            c, u = conn_for(url, 5)
            c.request("GET", u.path, headers={"Accept": "text/event-stream"})
            r = c.getresponse()
            if r.status != 200:
                drops.append((now(), f"status {r.status}"))
                time.sleep(0.05)
                continue
            while not STOP.is_set():
                line = r.fp.readline()
                if not line:
                    drops.append((now(), "eof"))
                    break
                if line.startswith(b"data:"):
                    events.append((now(), line[5:].strip().decode()))
        except Exception as e:  # noqa: BLE001
            drops.append((now(), type(e).__name__))
            time.sleep(0.05)


def ws_frame(payload):
    mask = os.urandom(4)
    data = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return bytes([0x81, 0x80 | len(payload)]) + mask + data


def ws(url, echoes, drops):
    u = urllib.parse.urlsplit(url)
    while not STOP.is_set():
        try:
            s = socket.create_connection((u.hostname, u.port or 80), timeout=5)
            key = base64.b64encode(os.urandom(16)).decode()
            s.sendall(
                (f"GET {u.path} HTTP/1.1\r\nHost: {u.hostname}\r\nUpgrade: websocket\r\n"
                 f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode()
            )
            head = b""
            while b"\r\n\r\n" not in head:
                chunk = s.recv(1024)
                if not chunk:
                    raise ConnectionError("closed during handshake")
                head += chunk
            if b" 101 " not in head.split(b"\r\n")[0]:
                raise ConnectionError(head.split(b"\r\n")[0].decode())
            i = 0
            while not STOP.is_set():
                msg = f"m{i}".encode()
                s.sendall(ws_frame(msg))
                hdr = s.recv(2)
                if len(hdr) < 2:
                    raise ConnectionError("closed")
                n = hdr[1] & 0x7F
                body = s.recv(n) if n else b""
                if body != msg:
                    raise ConnectionError(f"bad echo {body!r}")
                echoes.append(now())
                i += 1
                time.sleep(0.2)
        except Exception as e:  # noqa: BLE001
            drops.append((now(), type(e).__name__))
            time.sleep(0.05)


def longest_gap(times, start, end):
    pts = [start] + [t for t in times if start <= t <= end] + [end]
    return max(b - a for a, b in zip(pts, pts[1:]))


def summarize_poll(rows, t_trigger, t_end):
    window = [r for r in rows if r[0] >= t_trigger]
    fails = [r for r in window if not r[1]]
    longest, run_start = 0.0, None
    for r in window:
        if not r[1]:
            run_start = run_start if run_start is not None else r[0]
        elif run_start is not None:
            longest, run_start = max(longest, r[0] - run_start), None
    if run_start is not None:
        longest = max(longest, t_end - run_start)
    return {
        "requests": len(window),
        "failed": len(fails),
        "longest_failure_s": round(longest, 2),
        "max_latency_s": round(max((r[3] for r in window), default=0), 2),
        "errors": sorted({str(r[2]) for r in fails}),
        "upstreams": sorted({r[4] for r in window if r[4]}),
    }


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--name", required=True)
    p.add_argument("--poll", action="append", default=[])
    p.add_argument("--sse")
    p.add_argument("--ws")
    p.add_argument("--trigger", required=True)
    p.add_argument("--baseline", type=float, default=3)
    p.add_argument("--tail", type=float, default=10)
    a = p.parse_args()

    polls = {u: [] for u in a.poll}
    sse_events, sse_drops, ws_echoes, ws_drops = [], [], [], []
    threads = [threading.Thread(target=poller, args=(u, polls[u]), daemon=True) for u in a.poll]
    if a.sse:
        threads.append(threading.Thread(target=sse, args=(a.sse, sse_events, sse_drops), daemon=True))
    if a.ws:
        threads.append(threading.Thread(target=ws, args=(a.ws, ws_echoes, ws_drops), daemon=True))
    for t in threads:
        t.start()
    time.sleep(a.baseline)
    t_trigger = now()
    trig = subprocess.run(a.trigger, shell=True, capture_output=True, text=True)
    t_done = now()
    time.sleep(a.tail)
    t_end = now()
    STOP.set()

    result = {
        "name": a.name,
        "trigger_s": round(t_done - t_trigger, 2),
        "trigger_exit": trig.returncode,
        "trigger_out": (trig.stdout + trig.stderr).strip()[-300:],
        "poll": {u: summarize_poll(rows, t_trigger, t_end) for u, rows in polls.items()},
    }
    if a.sse:
        result["sse"] = {
            "drops": len([d for d in sse_drops if d[0] >= t_trigger]),
            "longest_gap_s": round(longest_gap([e[0] for e in sse_events], t_trigger, t_end), 2),
            "sources": sorted({e[1].split()[0] for e in sse_events if e[0] >= t_trigger}),
        }
    if a.ws:
        result["ws"] = {
            "drops": len([d for d in ws_drops if d[0] >= t_trigger]),
            "longest_gap_s": round(longest_gap(ws_echoes, t_trigger, t_end), 2),
        }
    print(json.dumps(result, indent=1))


if __name__ == "__main__":
    main()
