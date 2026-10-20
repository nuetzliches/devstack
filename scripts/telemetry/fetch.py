#!/usr/bin/env python3
"""Export a pool's telemetry (ADR-0037) from Elasticsearch to NDJSON files.

The pool's collector writes into two data streams, `logs-generic.otel-<ns>`
and `metrics-generic.otel-<ns>` (the elasticsearch exporter of the
OpenTelemetry collector, `mapping.mode: otel`). This script copies both to
`<out>/logs.ndjson` and `<out>/metrics.ndjson`, one `_source` per line, so
`analyze.py` can work offline and repeatably.

Two ways to reach Elasticsearch:

  --es URL                 directly (e.g. through an SSH tunnel); the user is
                           --user (default `elastic`), the password
                           $ES_PASSWORD.
  --ssh HOST --container C on HOST, inside container C (default
                           `elasticsearch`), with `curl` and the container's
                           own $ELASTIC_PASSWORD, which never appears on a
                           command line. Nothing leaves the host but the
                           documents.

Standard library only.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import shlex
import subprocess
import sys
import urllib.request
from pathlib import Path

PAGE = 5000
KEEP = "2m"


class Es:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args

    def call(self, method: str, path: str, body: dict | None = None) -> dict:
        data = json.dumps(body).encode() if body is not None else b""
        if self.args.es:
            req = urllib.request.Request(
                self.args.es.rstrip("/") + path, data=data or None, method=method
            )
            req.add_header("content-type", "application/json")
            password = os.environ.get("ES_PASSWORD", "")
            token = base64.b64encode(f"{self.args.user}:{password}".encode()).decode()
            req.add_header("authorization", f"Basic {token}")
            with urllib.request.urlopen(req) as r:
                return json.load(r)
        # Over SSH: the body goes through stdin, the password stays on the host
        # and off every command line. `printf` is a shell builtin; curl reads
        # the credentials as a config file from its stdin (`-K -`) and the
        # body from the original stdin, moved to fd 3.
        user = self.args.user.replace("\\", "\\\\").replace('"', '\\"')
        inner = (
            "exec 3<&0; "
            f'printf \'user = "{user}:%s"\\n\' "$ELASTIC_PASSWORD" | '
            f"curl -s -K - -X {method} -H content-type:application/json "
            f'"http://localhost:9200{path}" --data-binary @/dev/fd/3'
        )
        remote = f"docker exec -i {shlex.quote(self.args.container)} sh -c {shlex.quote(inner)}"
        cmd = ["ssh", "-o", "BatchMode=yes", self.args.ssh, remote]
        out = subprocess.run(cmd, input=data or b"{}", capture_output=True, check=True)
        return json.loads(out.stdout)


def export(es: Es, stream: str, query: dict, target: Path) -> int:
    n = 0
    page = es.call(
        "POST",
        f"/{stream}/_search?scroll={KEEP}",
        {"size": PAGE, "sort": ["_doc"], "query": query},
    )
    if "error" in page:
        if page.get("status") == 404:
            target.write_text("")
            return 0
        raise SystemExit(f"{stream}: {json.dumps(page['error'])[:500]}")
    with target.open("w", encoding="utf-8") as f:
        while True:
            hits = page["hits"]["hits"]
            if not hits:
                break
            for h in hits:
                f.write(json.dumps(h["_source"], ensure_ascii=False) + "\n")
            n += len(hits)
            page = es.call(
                "POST", "/_search/scroll", {"scroll": KEEP, "scroll_id": page["_scroll_id"]}
            )
    es.call("DELETE", "/_search/scroll", {"scroll_id": page["_scroll_id"]})
    return n


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--es", help="Elasticsearch URL, e.g. http://localhost:9201")
    p.add_argument("--user", default="elastic")
    p.add_argument("--ssh", help="SSH host that runs the Elasticsearch container")
    p.add_argument(
        "--container",
        default="elasticsearch",
        help="with --ssh: the Elasticsearch container's name (default: elasticsearch)",
    )
    p.add_argument("--namespace", default="devstack", help="the collector's tenant (data stream namespace)")
    p.add_argument("--since", help="only documents at or after this time (ES date math, e.g. now-2d)")
    p.add_argument("--out", type=Path, default=Path(".telemetry"), help="output directory")
    args = p.parse_args()
    if bool(args.es) == bool(args.ssh):
        p.error("give exactly one of --es and --ssh")

    query: dict = {"match_all": {}}
    if args.since:
        query = {"range": {"@timestamp": {"gte": args.since}}}
    args.out.mkdir(parents=True, exist_ok=True)
    es = Es(args)
    for kind in ("logs", "metrics"):
        stream = f"{kind}-generic.otel-{args.namespace}"
        n = export(es, stream, query, args.out / f"{kind}.ndjson")
        print(f"{stream}: {n} documents -> {args.out / (kind + '.ndjson')}", file=sys.stderr)


if __name__ == "__main__":
    main()
