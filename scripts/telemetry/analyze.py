#!/usr/bin/env python3
"""Summarise a pool's telemetry (ADR-0037) exported by `fetch.py`.

Reads `<dir>/logs.ndjson` and `<dir>/metrics.ndjson` and writes a Markdown
report: who sends what since when, gaps in the minute rounds, warnings and
errors, failed requests, durations (exact, from the log lines), service
start times derived from state changes, what made candidates build, a node's
capacity and lanes over time, and data-quality checks on the telemetry
itself. Facts only; judging them is for the reader.

Standard library only.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path

# A round is late when the next one comes later than this (the senders
# send every 60 s).
GAP_MS = 150_000
# Requests whose status is at least this are listed.
FAILED = 400


def load(path: Path) -> list[dict]:
    if not path.exists():
        return []
    with path.open(encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def ts(doc: dict) -> float:
    """`@timestamp` in ms; the exporter writes it as a number or a string."""
    return float(doc.get("@timestamp") or 0)


def when(ms: float) -> str:
    return datetime.fromtimestamp(ms / 1000, UTC).strftime("%Y-%m-%d %H:%M:%S")


def res(doc: dict, key: str) -> str:
    return str(doc.get("resource", {}).get("attributes", {}).get(key, "-"))


def attrs(doc: dict) -> dict:
    """The record's attributes without the `devstack.` prefix."""
    return {k.removeprefix("devstack."): v for k, v in doc.get("attributes", {}).items()}


def nested(value):
    """A nested object the exporter flattened to a JSON string, parsed."""
    if isinstance(value, str) and value.startswith(("{", "[")):
        try:
            return json.loads(value)
        except ValueError:
            pass
    return value


def member(doc: dict) -> str:
    return res(doc, "devstack.member.name")


def path_shape(path: str) -> str:
    """A request path with its ids replaced, so equal routes count together."""
    path = re.sub(r"/stacks/[0-9a-f]{8,}", "/stacks/{stack}", path)
    path = re.sub(r"/shared/[0-9a-f]{8,}", "/shared/{lease}", path)
    return path


def ms_text(ms: float) -> str:
    if ms >= 60_000:
        return f"{ms / 60_000:.1f} min"
    if ms >= 1000:
        return f"{ms / 1000:.1f} s"
    return f"{ms:.0f} ms"


def table(head: list[str], rows: list[list]) -> str:
    if not rows:
        return "_none_\n"
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    for r in rows:
        out.append("| " + " | ".join(str(c).replace("|", "\\|").replace("\n", " ") for c in r) + " |")
    return "\n".join(out) + "\n"


def senders(logs: list[dict], metrics: list[dict]) -> str:
    by: dict[tuple, list] = defaultdict(lambda: [0, 0, float("inf"), 0.0])
    for kind, docs in (("logs", logs), ("metrics", metrics)):
        for d in docs:
            k = (member(d), res(d, "service.name"), res(d, "service.version"), res(d, "os.type"))
            e = by[k]
            e[0 if kind == "logs" else 1] += 1
            e[2] = min(e[2], ts(d))
            e[3] = max(e[3], ts(d))
    rows = [[*k, e[0], e[1], when(e[2]), when(e[3])] for k, e in sorted(by.items())]
    return table(["member", "service", "version", "os", "logs", "metric docs", "first", "last"], rows)


def rounds(metrics: list[dict]) -> str:
    """Send rounds per sender (one process: member, service, repository)."""
    at: dict[tuple, set] = defaultdict(set)
    for d in metrics:
        # A histogram comes only with a round that had durations.
        if any(isinstance(v, dict) for v in d.get("metrics", {}).values()):
            continue
        repo = d.get("attributes", {}).get("devstack.repository", "")
        at[(member(d), res(d, "service.name"), repo)].add(ts(d))
    rows, gaps = [], []
    for k, s in sorted(at.items()):
        s = sorted(s)
        steps = [b - a for a, b in zip(s, s[1:])]
        median = statistics.median(steps) / 1000 if steps else None
        late = [(a, b) for a, b in zip(s, s[1:]) if b - a > GAP_MS]
        rows.append([*k, len(s), when(s[0]), when(s[-1]), f"{median:.0f} s" if median else "-", len(late)])
        gaps += [[*k, when(a), when(b), ms_text(b - a)] for a, b in late]
    out = table(["member", "service", "repository", "rounds", "first", "last", "median step", "gaps"], rows)
    if gaps:
        out += f"\nGaps longer than {GAP_MS // 1000} s (the process stopped, or could not send):\n\n"
        out += table(["member", "service", "repository", "from", "to", "silent"], gaps)
    return out


def events(logs: list[dict]) -> str:
    c = Counter((member(d), res(d, "service.name"), d.get("severity_text", "-"), d.get("body", {}).get("text", "-")) for d in logs)
    return table(["member", "service", "severity", "event", "count"], [[*k, n] for k, n in sorted(c.items(), key=lambda x: -x[1])])


def problems(logs: list[dict]) -> str:
    """Warnings and errors, grouped by what they say."""
    groups: dict[tuple, list] = defaultdict(list)
    for d in logs:
        if d.get("severity_number", 0) < 13:
            continue
        a = attrs(d)
        detail = a.get("error") or a.get("message") or a.get("why") or ""
        if "path" in a:
            detail = f"{a.get('method', '')} {path_shape(a['path'])} -> {a.get('status', '')}"
        key = (member(d), res(d, "service.name"), res(d, "service.version"), d.get("body", {}).get("text", "-"), str(detail)[:200])
        groups[key].append(ts(d))
    rows = [[*k, len(t), when(min(t)), when(max(t))] for k, t in sorted(groups.items(), key=lambda x: min(x[1]))]
    return table(["member", "service", "version", "event", "detail", "count", "first", "last"], rows)


def requests(logs: list[dict]) -> str:
    status = Counter()
    failed: dict[tuple, list] = defaultdict(list)
    for d in logs:
        a = attrs(d)
        if d.get("body", {}).get("text") != "request" or "status" not in a:
            continue
        s = int(a["status"])
        status[(res(d, "service.name"), s)] += 1
        if s >= FAILED or s == 502:
            failed[(member(d), res(d, "service.name"), a.get("method", ""), path_shape(a.get("path", "")), s)].append(ts(d))
    out = table(["service", "status", "count"], [[*k, n] for k, n in sorted(status.items())])
    out += "\nFailed requests:\n\n"
    out += table(
        ["member", "service", "method", "path", "status", "count", "first", "last"],
        [[*k, len(t), when(min(t)), when(max(t))] for k, t in sorted(failed.items(), key=lambda x: min(x[1]))],
    )
    return out


def durations(logs: list[dict], metrics: list[dict]) -> str:
    exact: dict[str, list] = defaultdict(list)
    detail = []
    for d in logs:
        a = attrs(d)
        if "ms" not in a:
            continue
        ev = d.get("body", {}).get("text", "-")
        exact[ev].append(float(a["ms"]))
        extra = {k: v for k, v in a.items() if k not in ("ms", "lease")}
        detail.append([when(ts(d)), member(d), ev, ms_text(float(a["ms"])), json.dumps(extra, ensure_ascii=False)])
    rows = [
        [ev, len(v), ms_text(min(v)), ms_text(statistics.median(v)), ms_text(max(v))]
        for ev, v in sorted(exact.items())
    ]
    out = "From the log lines (exact):\n\n" + table(["event", "n", "min", "median", "max"], rows)
    out += "\n" + table(["at", "member", "event", "duration", "with"], detail)
    hist = Counter()
    for d in metrics:
        h = d.get("metrics", {}).get("devstack.duration")
        if isinstance(h, dict):
            hist[attrs(d).get("event", "-")] += sum(h.get("counts", []))
    out += "\nHistograms `devstack.duration` in the metrics stream (count per event; "
    out += "the exporter keeps bucket counts only, not sum, min or max):\n\n"
    out += table(["event", "count"], sorted(hist.items()))
    return out


def service_starts(logs: list[dict]) -> str:
    """From `service` state changes: starting -> running/healthy/completed."""
    seq = sorted((d for d in logs if d.get("body", {}).get("text") == "service"), key=ts)
    open_: dict[tuple, float] = {}
    rows, failed = [], []
    for d in seq:
        a = attrs(d)
        status = nested(a.get("status"))
        state = status.get("state") if isinstance(status, dict) else None
        key = (member(d), a.get("lane", ""), a.get("service", "-"))
        if state == "starting":
            open_[key] = ts(d)
        elif state in ("running", "healthy", "completed") and key in open_:
            rows.append([when(open_[key]), key[0], key[2], state, ms_text(ts(d) - open_.pop(key))])
        elif state in ("failed", "crashed", "unhealthy"):
            failed.append([when(ts(d)), key[0], key[2], json.dumps(status)])
    out = table(["started", "member", "service", "reached", "after"], rows)
    out += "\nFailed starts:\n\n" + table(["at", "member", "service", "status"], failed)
    return out


def candidates(logs: list[dict]) -> str:
    """What made candidates build: the changed path that triggered them."""
    why: dict[str, set] = defaultdict(set)
    n = Counter()
    for d in logs:
        if d.get("body", {}).get("text") != "candidate":
            continue
        c = nested(attrs(d).get("candidate"))
        if isinstance(c, dict) and c.get("state") == "changed" and c.get("reason"):
            why[c["reason"]].add(attrs(d).get("service", "-"))
            n[c["reason"]] += 1
    rows = [[r, n[r], ", ".join(sorted(s))] for r, s in sorted(why.items(), key=lambda x: -n[x[0]])]
    return table(["changed path", "events", "services that got a candidate"], rows)


def series(points: list[tuple[float, float]]) -> list[list]:
    """Where a gauge changed its value: [from, to, value]."""
    out: list[list] = []
    for t, v in sorted(points):
        if out and out[-1][2] == v:
            out[-1][1] = t
        else:
            out.append([t, t, v])
    return out


def nodes(metrics: list[dict]) -> str:
    by: dict[tuple, list] = defaultdict(list)
    for d in metrics:
        for name, v in d.get("metrics", {}).items():
            if name.startswith("devstack.node.") and not isinstance(v, dict):
                by[(member(d), name, attrs(d).get("kind", ""))].append((ts(d), v))
    rows = []
    for (m, name, kind), pts in sorted(by.items()):
        vals = [v for _, v in pts]
        rows.append([m, name, kind, len(vals), f"{min(vals):.4g}", f"{statistics.median(vals):.4g}", f"{max(vals):.4g}"])
    out = table(["node", "metric", "kind", "points", "min", "median", "max"], rows)
    out += "\nLanes on a node over time:\n\n"
    lanes = []
    for (m, name, _), pts in sorted(by.items()):
        if name == "devstack.node.lanes":
            lanes += [[m, when(a), when(b), ms_text(b - a), f"{v:g}"] for a, b, v in series(pts)]
    return out + table(["node", "from", "to", "for", "lanes"], lanes)


def member_lanes(metrics: list[dict]) -> str:
    by: dict[tuple, list] = defaultdict(list)
    for d in metrics:
        v = d.get("metrics", {}).get("devstack.lanes")
        if v is None or isinstance(v, dict):
            continue
        a = attrs(d)
        by[(member(d), a.get("repository", "-"), a.get("place", "-"))].append((ts(d), v))
    rows = []
    for k, pts in sorted(by.items()):
        rows += [[*k, when(a), when(b), ms_text(b - a), f"{v:g}"] for a, b, v in series(pts) if v]
    return table(["member", "repository", "place", "from", "to", "for", "lanes"], rows)


def quality(logs: list[dict], metrics: list[dict]) -> str:
    """Checks on the telemetry itself."""
    rows = []
    anon = [d for d in logs + metrics if member(d) == "-"]
    if anon:
        rows.append(["documents without a member identity (not through the coordinator?)", len(anon),
                     ", ".join(sorted({res(d, "service.name") for d in anon}))])
    tests = [d for d in logs + metrics if "test" in res(d, "service.name")]
    if tests:
        rows.append(["documents from test senders", len(tests), ", ".join(sorted({res(d, "service.name") for d in tests}))])
    no_os = [d for d in logs if res(d, "os.type") == "-"]
    if no_os:
        rows.append(["log records without `os.type`", len(no_os), ""])
    no_repo = [d for d in logs if "repository" not in attrs(d)]
    if no_repo:
        rows.append(["log records without `devstack.repository`", len(no_repo), ""])
    strings = Counter(k for d in logs for k, v in attrs(d).items() if isinstance(v, str) and v.startswith(("{", "[")))
    if strings:
        rows.append(["attributes holding JSON as a string", sum(strings.values()), ", ".join(f"{k} ({n})" for k, n in strings.most_common())])
    empty = Counter(k for d in logs for k, v in attrs(d).items() if v == "")
    if empty:
        rows.append(["attributes with an empty value", sum(empty.values()), ", ".join(f"{k} ({n})" for k, n in empty.most_common())])
    unlabeled = [d for d in metrics if isinstance(d.get("metrics", {}).get("devstack.duration"), dict) and "repository" not in attrs(d)]
    if unlabeled:
        rows.append(["duration histograms without repository or lane", len(unlabeled), ""])
    observed = [d for d in logs if str(d.get("observed_timestamp", "")) in ("0", "0.0")]
    if observed:
        rows.append(["log records with `observed_timestamp` 0", len(observed), ""])
    return table(["check", "count", "which"], rows)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("dir", type=Path, nargs="?", default=Path(".telemetry"), help="what fetch.py wrote")
    p.add_argument("--out", type=Path, help="write the report here (default: stdout)")
    args = p.parse_args()
    logs = load(args.dir / "logs.ndjson")
    metrics = load(args.dir / "metrics.ndjson")
    if not logs and not metrics:
        sys.exit(f"no telemetry in {args.dir} (run fetch.py first)")
    span = [ts(d) for d in logs + metrics]
    parts = [
        "# Pool telemetry report\n",
        f"{len(logs)} log records, {len(metrics)} metric documents, {when(min(span))} to {when(max(span))} UTC.\n",
        "## Senders\n", senders(logs, metrics),
        "## Send rounds\n", rounds(metrics),
        "## Log events\n", events(logs),
        "## Warnings and errors\n", problems(logs),
        "## Requests\n", requests(logs),
        "## Durations\n", durations(logs, metrics),
        "## Service starts (from state changes)\n", service_starts(logs),
        "## What made candidates build\n", candidates(logs),
        "## Nodes\n", nodes(metrics),
        "## Lanes per member\n", member_lanes(metrics),
        "## Data quality\n", quality(logs, metrics),
    ]
    text = "\n".join(parts)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
        print(f"report -> {args.out}", file=sys.stderr)
    else:
        sys.stdout.reconfigure(encoding="utf-8")
        print(text)


if __name__ == "__main__":
    main()
