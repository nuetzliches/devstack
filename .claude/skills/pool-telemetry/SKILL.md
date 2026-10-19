---
name: pool-telemetry
description: Export and analyse the telemetry a devstack pool collected (ADR-0037) from Elasticsearch, and write findings. Use when asked to look at, evaluate or analyse pool telemetry, the collector's logs or metrics, what members sent, lane start durations seen in the pool, or "Telemetrie auswerten".
---

# Pool telemetry

A pool's coordinator forwards its members' telemetry (with their consent) to
an OTLP collector; with the OpenTelemetry collector's `elasticsearch`
exporter it lands in two data streams per tenant:

- `logs-generic.otel-<namespace>`: the diagnostic lines (`body.text` is the
  event, `attributes.devstack.*` its fields; nested values as dotted keys,
  `devstack.status.state`, from older senders as a JSON string under
  `devstack.status`; `devstack.repository` on every record of a daemon);
- `metrics-generic.otel-<namespace>`: gauges every 60 s per process
  (`devstack.lanes`, `devstack.services`, `devstack.tunnels`,
  `devstack.tunnel.bytes`, `devstack.node.*`) and `devstack.duration`
  histograms by event, repository, lane, node, place, service and result
  (bucket counts only; exact values are in the log lines' `ms`).

The sender is in `resource.attributes`: `devstack.member.name`/`.id` (set by
the coordinator from the certificate), `service.name` (`devstack-daemon`,
`devstack-hub`, `devstack-node`), `service.version`, `os.type`.

## Steps

1. **Export** (NDJSON into `.telemetry/`, which git ignores):

   ```sh
   # Elasticsearch in a container on a host you reach by SSH:
   python scripts/telemetry/fetch.py --ssh <host> --container <es-container>
   # or directly / through a tunnel (password in $ES_PASSWORD):
   python scripts/telemetry/fetch.py --es http://localhost:9201
   ```

   `--namespace` (default `devstack`) picks the tenant, `--since now-2d`
   limits the time. Where the collector of the maintainers' pool runs and how
   to reach it is not in this repository; check the agent's memory or ask.

2. **Report**: `python scripts/telemetry/analyze.py .telemetry --out .telemetry/report.md`.

3. **Read the raw log lines** too: the report groups; a sequence (what came
   right before a 502, which phases a lane start went through) needs the
   lines in order. A quick timeline:

   ```sh
   python -c "import json,sys;[print(d['@timestamp'],d['resource']['attributes'].get('devstack.member.name'),d['body']['text'],json.dumps(d.get('attributes',{}))) for d in sorted(map(json.loads,open('.telemetry/logs.ndjson',encoding='utf-8')),key=lambda d:float(d['@timestamp']))]"
   ```

4. **Explain each finding from the code** before writing it down: the event
   name leads to its `diag::info/warn` call (`rg '"<event>"' crates`). Check
   the versions involved (`service.version`; a node or coordinator may run an
   older build than the members: `git tag --contains <commit>`).

5. **Write the findings to a local, untracked file** (e.g.
   `TELEMETRY-<date>.local.md`, listed in `.git/info/exclude`): the data names
   members, repositories and services of downstream stacks and must not be
   committed (AGENTS.md, public-repository hygiene). Per finding: what was
   seen (time, member, version, numbers), the cause in the code if found
   (`file:line`), how sure, and what to do.

## What to look at

- Warnings and errors, failed requests (status >= 400, 502 from the hub).
- Durations: lane starts, syncs, moves; service start times from `service`
  state changes (`starting` -> `running`/`healthy`/`completed`).
- Candidates: which changed paths triggered builds (generated files that
  should be ignored by the watcher show up here).
- Send rounds: gaps, double senders, members that stopped sending.
- Version skew between members, nodes and the coordinator.
- The telemetry itself against ADR-0037 point 4 (labels, what is measured,
  nothing it must never contain).
