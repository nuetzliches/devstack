<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { paths, v1 } from '@/api';
import type { Run } from '@/api/types';
import { useSort } from '@/lib/sort';
import { ago, clock, duration, useNow } from '@/lib/format';
import { containerLook, statusLook, type StatusLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import LogView from '@/components/LogView.vue';
import StatusPill from '@/components/StatusPill.vue';
import SortTh from '@/components/SortTh.vue';
import Icon from '@/components/icons/Icon.vue';

const route = useRoute();
const router = useRouter();
const stack = useStack();
const now = useNow();
const running = ref(false);

const commands = computed(() => stack.view?.commands ?? []);
const selectedName = computed(() => (route.query.cmd as string | undefined) ?? commands.value[0]?.name ?? null);
const selected = computed(() => commands.value.find((c) => c.name === selectedName.value) ?? null);
const runs = computed<Run[]>(() => (selectedName.value ? (stack.runs[selectedName.value] ?? []) : []));
const runSort = useSort(
  runs,
  {
    id: (r) => r.id,
    status: (r) => r.status,
    started: (r) => r.started,
    duration: (r) => (r.finished ?? now.value) - r.started,
    error: (r) => r.error,
  },
  { id: 'command-runs' },
);
const runId = computed(() => (route.query.run ? Number(route.query.run) : (runs.value[0]?.id ?? null)));
const activeRun = computed(() => runs.value.find((r) => r.id === runId.value) ?? null);
const runLogPath = computed(() => (stack.id && stack.running && runId.value !== null ? paths.api(stack.id, v1.runLogStream(runId.value), stack.apiLane) : null));

// Recent runs: once when a command is opened (and when the stream comes back); events keep them current.
watch(
  () => [selectedName.value, stack.live] as const,
  ([name, live]) => {
    if (name && live) void stack.loadRuns(name);
  },
  { immediate: true },
);

function select(name: string) {
  void router.replace({ query: { cmd: name } });
}

function openRun(id: number) {
  void router.replace({ query: { cmd: selectedName.value ?? undefined, run: String(id) } });
}

async function run() {
  if (!selected.value) return;
  running.value = true;
  try {
    const id = await stack.runCommand(selected.value.name);
    if (id !== null) openRun(id);
  } finally {
    running.value = false;
  }
}

function requirement(name: string): StatusLook {
  const v = stack.view;
  const s = v?.services.find((x) => x.name === name);
  if (s) return statusLook(s.status);
  const c = v?.infra.flatMap((g) => g.containers).find((x) => x.service === name);
  if (c) return containerLook(c);
  return { label: 'unknown', tone: 'neutral', busy: false, detail: null };
}

function runTone(r: Run) {
  return r.status === 'succeeded' ? 'ok' : r.status === 'failed' ? 'danger' : 'accent';
}
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>Commands</h1>
        <p class="subtitle">One-off tasks from the stack file: seeds, resets, checks.</p>
      </div>
    </header>

    <div v-if="commands.length" class="layout stale-dim">
      <nav class="card cmd-list" aria-label="Commands">
        <button
          v-for="c in commands"
          :key="c.name"
          type="button"
          class="cmd-item"
          :class="{ active: c.name === selectedName }"
          :aria-current="c.name === selectedName ? 'true' : undefined"
          data-testid="command-item"
          :data-command="c.name"
          @click="select(c.name)"
        >
          <Icon name="terminal" :size="15" />
          <span class="grow">
            <span class="ci-name">{{ c.name }}</span>
            <span class="ci-desc">{{ c.description }}</span>
          </span>
          <Icon v-if="c.confirm" name="shield" :size="14" class="warnc" label="asks for confirmation" />
          <span v-if="stack.runs[c.name]?.[0]?.status === 'running'" class="spinner sm" aria-label="running" />
        </button>
      </nav>

      <div v-if="selected" class="detail">
        <section class="card">
          <div class="card-body cmd-head">
            <div class="grow">
              <h2 class="cmd-title">{{ selected.name }}</h2>
              <p class="muted">{{ selected.description }}</p>
              <div v-if="selected.requires.length" class="reqs">
                <span class="section-title">Requires</span>
                <span v-for="r in selected.requires" :key="r" class="req">
                  <span class="mono">{{ r }}</span>
                  <StatusPill :tone="requirement(r).tone" :label="requirement(r).label" :busy="requirement(r).busy" size="sm" />
                </span>
              </div>
              <p v-if="selected.confirm" class="confirm-note"><Icon name="shield" :size="14" /> Asks for confirmation: “{{ selected.confirm }}”</p>
            </div>
            <button v-if="!stack.readOnly" type="button" class="btn btn-primary" :disabled="running || stack.stale" data-testid="command-run" :data-command="selected.name" @click="run">
              <span v-if="running" class="spinner" /><Icon v-else name="play" />
              Run {{ selected.name }}
            </button>
          </div>
        </section>

        <section class="card log-card">
          <header class="card-header">
            <h2>{{ activeRun ? `Run #${activeRun.id}` : 'Run log' }}</h2>
            <StatusPill v-if="activeRun" :tone="runTone(activeRun)" :label="activeRun.status" :busy="activeRun.status === 'running'" size="sm" />
            <span v-if="activeRun" class="count">started {{ ago(activeRun.started, now) }}</span>
          </header>
          <LogView v-if="runLogPath" :key="runLogPath" :path="runLogPath" kind="run" height="320px" testid="run-log" empty-text="No output." />
          <div v-else class="empty">
            <Icon name="terminal" :size="28" />
            <p v-if="!stack.running">The stack is not running.</p>
            <p v-else>Run the command to see its output here.</p>
          </div>
        </section>

        <section class="card">
          <header class="card-header">
            <h2>Recent runs</h2>
            <span class="count">{{ runs.length }}</span>
          </header>
          <div v-if="runs.length" class="table-wrap">
            <table class="table" data-testid="runs-table">
              <thead>
                <tr>
                  <SortTh label="Run" :dir="runSort.dirOf('id')" @toggle="runSort.toggle('id')" />
                  <SortTh label="Status" :dir="runSort.dirOf('status')" @toggle="runSort.toggle('status')" />
                  <SortTh label="Started" :dir="runSort.dirOf('started')" @toggle="runSort.toggle('started')" />
                  <SortTh label="Duration" class="num" :dir="runSort.dirOf('duration')" @toggle="runSort.toggle('duration')" />
                  <SortTh label="Error" class="hide-sm" :dir="runSort.dirOf('error')" @toggle="runSort.toggle('error')" />
                </tr>
              </thead>
              <tbody>
                <tr v-for="r in runSort.sorted.value" :key="r.id" class="run-row" :class="{ sel: r.id === runId }" tabindex="0" @click="openRun(r.id)" @keydown.enter="openRun(r.id)">
                  <td class="mono">#{{ r.id }}</td>
                  <td><StatusPill :tone="runTone(r)" :label="r.status" :busy="r.status === 'running'" size="sm" /></td>
                  <td class="nowrap" :title="new Date(r.started).toLocaleString()">{{ clock(r.started) }} <span class="faint">· {{ ago(r.started, now) }}</span></td>
                  <td class="num">{{ r.finished ? duration(r.finished - r.started) : duration(now - r.started) }}</td>
                  <td class="hide-sm err truncate">{{ r.error ?? '' }}</td>
                </tr>
              </tbody>
            </table>
          </div>
          <div v-else class="empty"><p>No runs yet.</p></div>
        </section>
      </div>
    </div>
    <div v-else class="card">
      <div class="empty">
        <Icon name="terminal" :size="32" />
        <h2>No commands</h2>
        <p>Commands are declared in the stack file under <code>[commands]</code>.</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.layout {
  display: grid;
  grid-template-columns: 280px minmax(0, 1fr);
  gap: 16px;
  align-items: start;
}
@media (max-width: 860px) {
  .layout {
    grid-template-columns: minmax(0, 1fr);
  }
}
.cmd-list {
  padding: 6px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.cmd-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 10px;
  border: 0;
  background: transparent;
  border-radius: 8px;
  text-align: left;
  cursor: pointer;
  color: var(--text-2);
}
.cmd-item:hover {
  background: var(--surface-hover);
}
.cmd-item.active {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.ci-name {
  display: block;
  font-weight: 600;
  color: var(--text);
}
.ci-desc {
  display: block;
  font-size: 12px;
  color: var(--text-muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.warnc {
  color: var(--warn-text);
}
.spinner.sm {
  width: 12px;
  height: 12px;
  color: var(--accent);
}
.detail {
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
}
.cmd-head {
  display: flex;
  gap: 16px;
  align-items: flex-start;
  flex-wrap: wrap;
}
.cmd-title {
  font-size: 17px;
  font-family: var(--font-mono);
}
.reqs {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-top: 10px;
}
.req {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.confirm-note {
  display: flex;
  align-items: flex-start;
  gap: 6px;
  margin-top: 10px;
  font-size: 12.5px;
  color: var(--warn-text);
}
.confirm-note svg {
  margin-top: 2px;
  flex: none;
}
.log-card {
  overflow: hidden;
}
.run-row {
  cursor: pointer;
}
.run-row.sel td {
  background: var(--accent-soft);
}
.err {
  color: var(--danger-text);
  max-width: 260px;
}
@media (max-width: 520px) {
  .hide-sm {
    display: none;
  }
}
</style>
