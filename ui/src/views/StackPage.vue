<script setup lang="ts">
// A stack's own page, above its lanes (ADR-0081): what is the stack's and
// not one lane's. Its daemon (state, restart banner), the stack's actions
// (start, stop, restart: every lane), who serves the main ports and the
// control to give them to another lane, its lanes (`main`, the main working
// tree, first; a row opens the lane's views; no lane actions here), the
// services that run once per machine (`singleton`, ADR-0080) and what each
// lane uses. A stack that does not run shows its start and its worktrees as
// the hub lists them (ADR-0078). It follows the main tree's stream, without
// making `main` the lane an older link opens.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { api, paths, v1 } from '@/api';
import type { LaneView, Load, UsageReport } from '@/api/types';
import { ago, bytes, clock, duration, percent, useNow } from '@/lib/format';
import { viaText } from '@/lib/journal';
import { laneLabel, lastTab, MAIN, nodeLabel, restartLanesText, started } from '@/lib/lanes';
import { laneView } from '@/lib/stackRoutes';
import { statusLook } from '@/lib/status';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import AgentPrompt from '@/components/AgentPrompt.vue';
import Icon from '@/components/icons/Icon.vue';
import StatusPill from '@/components/StatusPill.vue';

const route = useRoute();
const router = useRouter();
const hub = useHub();
const stack = useStack();
const now = useNow();

watch(
  () => route.params.stack,
  (id) => {
    if (typeof id !== 'string' || !id) return;
    stack.select(id, { lane: null, remember: false });
    // Someone else's lane (ADR-0040) has no stack here: its views only.
    if (stack.foreign) void router.replace(laneView(id, null));
  },
  { immediate: true },
);

const name = computed(() => stack.entry?.name ?? stack.mainView?.name ?? 'Stack');
const repo = computed(() => stack.entry?.repo ?? stack.mainView?.repo ?? '');
const daemon = computed(() => stack.entry?.daemon ?? null);
const hubDown = computed(() => hub.status === 'unreachable' || hub.stream === 'error');
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
const changes = computed(() => !hubDown.value && !stack.readOnly && !!stack.entry);

watch(
  () => [name.value, route.name] as const,
  ([n]) => {
    if (route.name === 'stack') document.title = `${n} · devstack`;
  },
  { immediate: true },
);

/** The daemon's state: running (pid, port, uptime), idle (nothing runs, it stays for nothing), or offline. */
const state = computed(() => {
  if (hubDown.value) return { tone: '', label: 'unknown', title: 'The hub is not reachable' };
  if (!stack.running) return { tone: '', label: 'offline', title: 'Its daemon does not run' };
  if (!stack.mainActive && !stack.runningLanes.length) return { tone: '', label: 'idle', title: 'Its daemon runs; nothing of its lanes does' };
  return { tone: 'ok', label: 'running', title: 'Its daemon runs' };
});

// The restart banner (version, changed stack file), from the lane view to here.
const daemonOutdated = computed(() => {
  const d = stack.entry?.daemon?.version;
  const h = hub.version;
  return d && h && d !== h ? { daemon: d, hub: h } : null;
});
const restartReason = computed(() => {
  if (!stack.running || hubDown.value) return null;
  if (daemonOutdated.value) return 'version';
  if (stack.mainView?.config_changed) return 'config';
  return null;
});

// ------------------------------------------------------------ lanes

/** The lanes, `main` first, then by label; of a stack that does not run, its worktrees as the hub lists them. */
const lanes = computed<LaneView[]>(() => {
  const own = stack.running ? (stack.lanesOf === stack.id ? stack.lanes : []) : stack.offlineLanesOf === stack.id ? stack.offlineLanes : [];
  const rest = [...own].sort((a, b) => laneLabel(a).localeCompare(laneLabel(b)));
  return stack.mainLane ? [stack.mainLane, ...rest] : rest;
});

/** What each started lane uses now (its own daemon's `/v1/usage`), read every 10 s while the page is shown. */
const usage = ref<Record<string, Load & { disk?: number }>>({});
async function loadUsage() {
  const sid = stack.id;
  if (!sid || !stack.running || stack.foreign) return;
  const out: Record<string, Load & { disk?: number }> = {};
  await Promise.all(
    lanes.value.filter((l) => started(l) && !l.moving).map(async (l) => {
      try {
        const res = await api().request<UsageReport>('GET', paths.api(sid, v1.usageNow, l.name === MAIN ? null : l.name));
        const own = res.ok && res.body && typeof res.body === 'object' && Array.isArray(res.body.lanes) ? res.body.lanes[0] : null;
        if (own) {
          const d = own.disk;
          out[l.name] = { cpu: own.cpu, memory: own.memory, ...(d ? { disk: d.snapshot + d.artifacts + d.logs + (d.volumes ?? 0) } : {}) };
        }
      } catch {
        /* not known */
      }
    }),
  );
  if (sid === stack.id) usage.value = out;
}
const timer = setInterval(() => void loadUsage(), 10_000);
onBeforeUnmount(() => clearInterval(timer));
watch(() => [stack.id, stack.running, lanes.value.filter(started).map((l) => l.name).join()] as const, () => void loadUsage(), { immediate: true });
/** What the stack's lanes use together, of those known. */
const total = computed(() => {
  const xs = Object.values(usage.value);
  return xs.length ? { cpu: xs.reduce((n, x) => n + x.cpu, 0), memory: xs.reduce((n, x) => n + x.memory, 0), lanes: xs.length } : null;
});

function laneState(l: LaneView): { dot: string; label: string } {
  if (l.removing) return { dot: 'warn', label: 'removing' };
  if (l.moving?.error) return { dot: 'err', label: 'start failed' };
  if (l.moving) return { dot: 'warn', label: l.moving.kind === 'move' ? 'moving' : 'starting' };
  if (!started(l)) return { dot: 'off', label: 'not started' };
  if (l.node_error || l.notices?.length) return { dot: 'warn', label: 'running' };
  return { dot: 'on', label: 'running' };
}
function open(l: LaneView) {
  if (stack.id) void router.push(laneView(stack.id, l.name, lastTab(route.name)));
}

// ------------------------------------------------------------ main slot

/** The lane on the main ports: a lane that holds them, else `main`. */
const servedBy = computed(() => lanes.value.find((l) => l.owns_main) ?? null);
const servedNote = computed(() => {
  const m = stack.mainView?.main_served;
  if (!m || !servedBy.value || m.lane !== servedBy.value.name) return '';
  return `${m.at ? `since ${clock(m.at)}, ` : ''}set ${viaText(m.via)}`;
});
/** Where the main ports can go: the started lanes on this machine or a dedicated node, `main` whether it runs or not. */
const takers = computed(() => lanes.value.filter((l) => !l.owns_main && (l.name === MAIN || (started(l) && !l.moving && !l.removing))));
const giveTo = ref('');
watch(takers, (t) => {
  if (!t.some((l) => l.name === giveTo.value)) giveTo.value = t[0]?.name ?? '';
});
const busyMain = computed(() => !!stack.busy['lane-main']);
const noMainPorts = computed(() => !!stack.mainView?.no_main_ports);

// ------------------------------------------------------------ singletons

const services = computed(() => stack.mainView?.services ?? []);
/** An older daemon does not say which services are singletons. */
const singletonsKnown = computed(() => services.value.some((s) => typeof s.singleton === 'boolean'));
const singletons = computed(() => services.value.filter((s) => s.singleton));
</script>

<template>
  <div class="page stack-page" data-testid="stack-page">
    <header class="page-header">
      <div class="head">
        <h1 class="truncate">{{ name }}</h1>
        <p class="subtitle mono truncate" :title="repo">{{ repo }}</p>
        <p class="daemon small">
          <span class="pill" :class="state.tone" :title="state.title" data-testid="stack-state"><span class="dot" />{{ state.label }}</span>
          <template v-if="daemon && stack.running">
            <span class="muted" data-testid="stack-daemon">daemon pid {{ daemon.pid }} · port {{ daemon.port }} · up {{ duration(now - daemon.started) }}</span>
            <span class="tag" title="daemon version">devstack {{ daemon.version }}</span>
          </template>
          <span v-else-if="stack.entry?.last_seen" class="muted">last seen {{ ago(stack.entry.last_seen, now) }}</span>
        </p>
      </div>
      <div v-if="changes" class="actions">
        <template v-if="!stack.running">
          <button type="button" class="btn btn-primary" :disabled="starting" title="Its daemon, and the lane main: its infrastructure and autostart services" data-testid="action-stack-start" @click="stack.startStack()">
            <span v-if="starting" class="spinner" /><Icon v-else name="play" :size="14" /> {{ starting ? 'Starting…' : 'Start stack' }}
          </button>
        </template>
        <template v-else>
          <button type="button" class="btn" :disabled="!!stack.busy.stack" title="Stop every lane, then start the stack again with the installed devstack and the current stack file" data-testid="action-restart-stack" @click="stack.restartStack()">
            <span v-if="stack.busy.stack === 'restart'" class="spinner" /><Icon v-else name="restart" :size="14" /> Restart stack
          </button>
          <button type="button" class="btn btn-stop" :disabled="!!stack.busy.stack" title="Every lane stops, main included, and the daemon exits" data-testid="action-stack-stop" @click="stack.stopStack()">
            <span v-if="stack.busy.stack === 'stop'" class="spinner" /><Icon v-else name="power" :size="14" /> Stop stack
          </button>
        </template>
      </div>
    </header>

    <div v-if="restartReason" class="banner warn" :data-testid="restartReason === 'version' ? 'daemon-outdated' : 'config-changed'">
      <Icon :name="restartReason === 'version' ? 'alert' : 'file'" :size="18" />
      <span v-if="restartReason === 'version'" class="grow">
        <strong>{{ name }} runs devstack {{ daemonOutdated?.daemon }}, devstack {{ daemonOutdated?.hub }} is installed.</strong>
        {{ ' ' }}A restart of the stack switches it over<template v-if="stack.mainView?.config_changed"> and applies the changed stack file</template>.
        <template v-if="restartLanesText(stack.restartLanes)">{{ ' ' }}{{ restartLanesText(stack.restartLanes) }}</template>
      </span>
      <span v-else class="grow"><strong>The stack file changed since {{ name }} started.</strong> It applies after a restart.</span>
      <button v-if="changes" type="button" class="btn btn-primary btn-sm" :disabled="!!stack.busy.stack" data-testid="banner-restart-stack" @click="stack.restartStack()">
        <span v-if="stack.busy.stack === 'restart'" class="spinner" /><Icon v-else name="restart" :size="14" />
        {{ stack.busy.stack === 'restart' ? 'Restarting…' : 'Restart stack' }}
      </button>
    </div>
    <div v-if="!hubDown && stack.entry?.start_error && !stack.running" class="banner warn" data-testid="stack-start-error">
      <Icon name="alert" :size="18" />
      <span class="grow">
        <strong>The last start failed:</strong>
        <pre class="start-error">{{ stack.entry.start_error }}</pre>
      </span>
      <AgentPrompt v-if="changes && stack.id" kind="start-failed" :target="{ stack: stack.id }" />
    </div>

    <section v-if="stack.running" class="card" data-testid="stack-main-slot">
      <header class="card-header">
        <Icon name="promote" :size="16" />
        <h2>Main ports</h2>
      </header>
      <div class="card-body slot">
        <p v-if="noMainPorts" class="muted" data-testid="stack-no-main-ports">No service of the stack listens on a fixed port or runs workers: nothing moves with the main ports.</p>
        <template v-else>
          <p data-testid="stack-main-served">
            <template v-if="servedBy">
              <strong>{{ laneLabel(servedBy) }}</strong> serves the main ports<template v-if="servedBy.name === MAIN"> (its own)</template>
              <span v-if="servedNote" class="muted">, {{ servedNote }}</span>.
            </template>
            <template v-else>Not known yet.</template>
          </p>
          <form v-if="changes && takers.length" class="give" @submit.prevent="stack.laneMain(giveTo)">
            <label class="muted small" for="give-to">Give them to</label>
            <select id="give-to" v-model="giveTo" class="input" data-testid="stack-main-to">
              <option v-for="l in takers" :key="l.name" :value="l.name">{{ laneLabel(l) }}{{ l.node ? ` · on ${nodeLabel(l)}` : '' }}</option>
            </select>
            <button type="submit" class="btn btn-sm" :disabled="busyMain || !giveTo" title="Its services, dev servers included, answer on the main ports; workers move with them" data-testid="stack-main-give">
              <span v-if="busyMain" class="spinner" /><Icon v-else name="promote" :size="14" /> Give
            </button>
            <button
              v-if="servedBy && servedBy.name !== MAIN"
              type="button"
              class="btn btn-sm"
              :disabled="busyMain"
              title="The main working tree serves its ports again"
              data-testid="stack-main-back"
              @click="stack.laneMain(MAIN)"
            >
              <Icon name="rollback" :size="14" /> Back to main
            </button>
          </form>
        </template>
      </div>
    </section>

    <section class="card" data-testid="stack-lanes">
      <header class="card-header">
        <Icon name="gitBranch" :size="16" />
        <h2>{{ stack.running ? 'Lanes' : 'Worktrees' }}</h2>
        <span class="count">{{ lanes.length }}</span>
        <span class="grow" />
        <span v-if="total" class="muted small" data-testid="stack-usage-total">{{ percent(total.cpu) }} CPU · {{ bytes(total.memory) }} in {{ total.lanes }} lane{{ total.lanes === 1 ? '' : 's' }}</span>
      </header>
      <div class="table-wrap">
        <table class="table lanes">
          <thead>
            <tr>
              <th>Lane</th>
              <th>Branch</th>
              <th>Where</th>
              <th>State</th>
              <th class="num">Ports</th>
              <th class="num">CPU</th>
              <th class="num">Memory</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="l in lanes" :key="l.name" class="row" tabindex="0" :title="l.path" data-testid="stack-lane" :data-lane="l.name" @click="open(l)" @keydown.enter="open(l)">
              <td>
                <RouterLink :to="laneView(stack.id ?? '', l.name, lastTab(route.name))" class="lane-name" @click.stop>{{ l.name === MAIN ? MAIN : l.name }}</RouterLink>
                <span v-if="l.name === MAIN" class="faint small"> main working tree</span>
                <span v-if="l.owns_main && stack.running" class="pill sm ok" title="It serves the main ports" data-testid="stack-lane-main-ports">main ports</span>
              </td>
              <td class="mono small">{{ l.branch ?? (l.name === MAIN ? '' : 'detached') }}</td>
              <td class="small">
                <span v-if="l.node" class="pill sm info">{{ nodeLabel(l) }}</span>
                <span v-else class="muted">here</span>
              </td>
              <td class="small">
                <span class="state"><span class="dot" :class="laneState(l).dot" aria-hidden="true" />{{ laneState(l).label }}</span>
              </td>
              <td class="num small tabular">+{{ l.port_offset }}</td>
              <td class="num small tabular">{{ usage[l.name] ? percent(usage[l.name].cpu) : '' }}</td>
              <td class="num small tabular" data-testid="stack-lane-memory">{{ usage[l.name] ? bytes(usage[l.name].memory) : '' }}</td>
            </tr>
            <tr v-if="!lanes.length">
              <td colspan="7" class="muted">No worktree known yet.</td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section v-if="stack.mainView" class="card" data-testid="stack-singletons">
      <header class="card-header">
        <Icon name="server" :size="16" />
        <h2>Once per machine</h2>
        <span v-if="singletonsKnown" class="count">{{ singletons.length }}</span>
        <span class="faint small">singleton services: in the lane main only</span>
      </header>
      <div v-if="!singletonsKnown" class="card-body muted small">This daemon does not say which services run once per machine (devstack before 0.33.0).</div>
      <div v-else-if="!singletons.length" class="card-body muted small">None: every service runs in each lane.</div>
      <ul v-else class="singletons">
        <li v-for="s in singletons" :key="s.name" data-testid="stack-singleton">
          <RouterLink :to="laneView(stack.id ?? '', MAIN, 'service', { name: s.name })" class="lane-name">{{ s.name }}</RouterLink>
          <span class="faint small">{{ s.kind }}<template v-if="s.port"> · :{{ s.port }}</template></span>
          <span class="grow" />
          <StatusPill size="sm" :tone="statusLook(s.status).tone" :label="statusLook(s.status).label" />
        </li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.head {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
h1 {
  font-size: 20px;
  margin: 0;
}
.daemon {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin: 6px 0 0;
}
.actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.small {
  font-size: 12px;
}
.slot p {
  margin: 0 0 10px;
}
.give {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.give .input {
  min-width: 200px;
  width: auto;
}
.lanes .row {
  cursor: pointer;
}
.lanes .row:focus-visible td {
  box-shadow: inset var(--focus);
}
.lane-name {
  font-weight: 600;
  color: inherit;
  text-decoration: none;
  margin-right: 6px;
}
.lane-name:hover {
  text-decoration: underline;
}
.state {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex: none;
}
.dot.on {
  background: var(--ok);
}
.dot.warn {
  background: var(--warn);
}
.dot.err {
  background: var(--danger);
}
.dot.off {
  border: 1.5px solid var(--text-faint);
}
.singletons {
  list-style: none;
  margin: 0;
  padding: 4px 0;
}
.singletons li {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px var(--space-4);
}
.singletons li + li {
  border-top: 1px solid var(--border);
}
.grow {
  flex: 1;
  min-width: 0;
}
.start-error {
  margin: 4px 0 0;
  padding: 8px 10px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--warn) 10%, transparent);
  font-size: 12px;
  white-space: pre-wrap;
  max-height: 14em;
  overflow: auto;
}
</style>
