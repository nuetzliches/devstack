<script setup lang="ts">
import { computed, ref } from 'vue';
import { useRoute } from 'vue-router';
import { paths, v1, type ServiceAction } from '@/api';
import { ago, bytes, dateTime, percent, serviceUrl, shortBuild, useNow } from '@/lib/format';
import { kindIcon, kindLabel, statusLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import StatusPill from '@/components/StatusPill.vue';
import CandidatePanel from '@/components/CandidatePanel.vue';
import LogView from '@/components/LogView.vue';
import BuildLogModal from '@/components/BuildLogModal.vue';
import KeptLogsModal from '@/components/KeptLogsModal.vue';
import BuildTimeline from '@/components/BuildTimeline.vue';
import Sparkline from '@/components/Sparkline.vue';
import Icon from '@/components/icons/Icon.vue';
import type { IconName } from '@/components/icons/icons';

const route = useRoute();
const stack = useStack();
const now = useNow();
const showBuildLog = ref(false);
const showKeptLogs = ref(false);

const name = computed(() => route.params.name as string);
const svc = computed(() => stack.view?.services.find((s) => s.name === name.value) ?? null);
const look = computed(() => (svc.value ? statusLook(svc.value.status) : null));
const busy = computed(() => stack.busy[`svc:${name.value}`]);
const state = computed(() => svc.value?.status.state ?? 'stopped');
const isDown = computed(() => ['stopped', 'completed', 'exited', 'failed'].includes(state.value));

const logPath = computed(() => (stack.id && stack.running && stack.live && svc.value ? paths.api(stack.id, v1.logStream(name.value, 500)) : null));

const series = computed(() => stack.history.get(`svc:${name.value}`) ?? []);
const cpuValues = computed(() => series.value.map((p) => p.cpu));
const memValues = computed(() => series.value.map((p) => p.memory));
const current = computed(() => stack.usage?.services.find((s) => s.name === name.value) ?? null);
const peakCpu = computed(() => Math.max(0, ...cpuValues.value));
const peakMem = computed(() => Math.max(0, ...memValues.value));

type Act = { action: ServiceAction; label: string; icon: IconName; show: boolean; primary?: boolean; title: string };
const actions = computed<Act[]>(() => {
  const s = svc.value;
  if (!s) return [];
  return [
    { action: 'start', label: 'Start', icon: 'play', show: isDown.value, primary: true, title: 'Start the service from its current build' },
    { action: 'restart', label: 'Restart', icon: 'restart', show: !isDown.value, title: 'Rebuild; the running instance is replaced only if the build succeeds and is healthy' },
    // promote lives in the candidate panel, next to the reason it is held
    { action: 'rollback', label: 'Roll back', icon: 'rollback', show: !!s.build_id && !isDown.value, title: 'Switch back to the previous build' },
    { action: 'stop', label: 'Stop', icon: 'stop', show: !isDown.value, title: 'Stop the service' },
  ];
});

const kv = computed(() => {
  const s = svc.value;
  if (!s) return [];
  return [
    { k: 'Running build', v: s.build_id ?? '—', mono: true, testid: 'detail-build' },
    { k: 'Since', v: `${ago(s.since, now.value)} (${dateTime(s.since)})` },
    { k: 'Kind', v: kindLabel(s) },
    { k: 'Process', v: s.pid ? `pid ${s.pid}` : '—' },
    { k: 'Fixed port', v: s.port ? String(s.port) : '—', mono: true },
    { k: 'Instance port', v: s.instance_port ? String(s.instance_port) : s.proxy ? '—' : 'same as fixed (no proxy)', mono: !!s.instance_port },
    { k: 'Autostart', v: s.autostart ? 'yes' : 'no' },
    { k: 'Groups', v: s.groups.length ? s.groups.join(', ') : '—' },
    { k: 'Log file', v: s.log ?? '—', mono: true },
  ];
});
</script>

<template>
  <div class="page">
    <RouterLink :to="{ name: 'overview', params: { stack: stack.id ?? '' } }" class="back"><Icon name="chevronLeft" :size="14" /> Overview</RouterLink>

    <template v-if="svc && look">
      <header class="page-header" data-testid="service-detail" :data-service="svc.name">
        <div class="row head-left">
          <span class="kind"><Icon :name="kindIcon(svc)" :size="20" /></span>
          <div>
            <div class="row wrap">
              <h1>{{ svc.name }}</h1>
              <StatusPill :tone="look.tone" :label="look.label" :busy="look.busy" data-testid="detail-status" />
            </div>
            <div class="subtitle">
              {{ kindLabel(svc) }}
              <template v-if="svc.build_id"> · running build <span class="mono">{{ shortBuild(svc.build_id) }}</span></template>
              <template v-if="svc.port"> · <a :href="serviceUrl(svc)" target="_blank" rel="noopener" data-testid="service-url">{{ serviceUrl(svc).replace(/^https?:\/\//, '') }} <Icon name="external" :size="12" /></a></template>
            </div>
          </div>
        </div>
        <div class="btn-group">
          <template v-for="a in actions" :key="a.action">
            <button
              v-if="a.show"
              type="button"
              class="btn"
              :class="{ 'btn-primary': a.primary && a.action !== 'promote', 'btn-warn': a.action === 'promote' }"
              :disabled="!!busy || stack.stale"
              :title="a.title"
              :data-testid="`action-${a.action}`"
              @click="stack.serviceAction(svc.name, a.action)"
            >
              <span v-if="busy === a.action" class="spinner" /><Icon v-else :name="a.icon" />
              {{ busy === a.action ? `${a.label}…` : a.label }}
            </button>
          </template>
        </div>
      </header>

      <div v-if="look.detail" class="banner warn">
        <Icon name="alert" :size="18" />
        <span class="grow">{{ look.detail }}</span>
      </div>

      <CandidatePanel v-if="svc.candidate" class="stale-dim" :service="svc" @build-log="showBuildLog = true" />
      <div v-else class="live-line" data-testid="live-line">
        <Icon name="checkCircle" :size="16" />
        <span v-if="svc.build_id">The latest build <span class="mono">{{ shortBuild(svc.build_id) }}</span> is the one running. No change is on its way.</span>
        <span v-else>No build is running.</span>
      </div>

      <div class="cols stale-dim">
        <section class="card">
          <header class="card-header"><h2>Details</h2></header>
          <div class="card-body">
            <dl class="kv">
              <template v-for="row in kv" :key="row.k">
                <dt>{{ row.k }}</dt>
                <dd :class="{ mono: row.mono }" :data-testid="row.testid">{{ row.v }}</dd>
              </template>
            </dl>
          </div>
        </section>

        <div class="side">
          <section v-if="svc.proxy" class="card" data-testid="proxy-card">
            <header class="card-header">
              <h2>Proxy</h2>
              <span class="count">:{{ svc.proxy.port }} → {{ svc.proxy.upstream ?? '—' }}</span>
            </header>
            <div class="card-body">
              <div class="proxy-flow">
                <div class="node"><span class="nl">fixed</span><span class="mono">:{{ svc.proxy.port }}</span></div>
                <span class="arrow" aria-hidden="true">→</span>
                <div class="node" :class="{ dim: !svc.proxy.upstream }"><span class="nl">instance</span><span class="mono">{{ svc.proxy.upstream ? `:${svc.proxy.upstream}` : 'none' }}</span><span class="mono faint">{{ shortBuild(svc.proxy.build) }}</span></div>
                <template v-if="svc.proxy.pending">
                  <span class="arrow dashed" aria-hidden="true">⇢</span>
                  <div class="node pending"><span class="nl">pending</span><span class="mono">{{ shortBuild(svc.proxy.pending) }}</span></div>
                </template>
              </div>
              <dl class="kv proxy-kv">
                <dt>Active connections</dt>
                <dd class="tabular">{{ svc.proxy.active }}</dd>
                <dt>Requests served</dt>
                <dd class="tabular">{{ svc.proxy.served.toLocaleString() }}</dd>
                <dt>Last error</dt>
                <dd :class="svc.proxy.last_error ? 'err' : 'faint'">{{ svc.proxy.last_error ?? 'none' }}</dd>
              </dl>
            </div>
          </section>

          <section class="card" data-testid="resources-card">
            <header class="card-header">
              <h2>Resources</h2>
              <span class="count">last 10 min</span>
            </header>
            <div class="card-body res">
              <div class="metric">
                <div class="mh">
                  <span class="ml"><Icon name="cpu" :size="14" /> CPU</span>
                  <span class="mv tabular">{{ percent(current?.cpu) }}</span>
                </div>
                <Sparkline :values="cpuValues" :width="300" :height="44" :min-max="5" fluid color="var(--accent)" :label="`CPU history, peak ${percent(peakCpu)}`" />
                <div class="mf faint">peak {{ percent(peakCpu) }}<template v-if="current?.processes !== undefined"> · {{ current.processes }} process{{ current.processes === 1 ? '' : 'es' }}</template></div>
              </div>
              <div class="metric">
                <div class="mh">
                  <span class="ml"><Icon name="hardDrive" :size="14" /> Memory</span>
                  <span class="mv tabular">{{ bytes(current?.memory) }}</span>
                </div>
                <Sparkline :values="memValues" :width="300" :height="44" :min-max="8 * 1024 * 1024" :zero-based="false" fluid color="var(--ok)" :label="`Memory history, peak ${bytes(peakMem)}`" />
                <div class="mf faint">peak {{ bytes(peakMem) }}</div>
              </div>
            </div>
          </section>
        </div>
      </div>

      <BuildTimeline v-if="svc.build_id || svc.candidate" class="stale-dim" :service="svc.name" :build="svc.build_id" :candidate-state="svc.candidate?.state ?? null" />

      <section class="card log-card">
        <header class="card-header">
          <h2>Log</h2>
          <span class="count">current instance, live</span>
          <span class="grow" />
          <button type="button" class="btn btn-sm" data-testid="action-kept-logs" title="What earlier instances printed, by start" @click="showKeptLogs = true"><Icon name="history" :size="14" /> Earlier logs</button>
          <button type="button" class="btn btn-sm" data-testid="action-build-log" @click="showBuildLog = true"><Icon name="file" :size="14" /> Build log</button>
        </header>
        <LogView v-if="logPath" :path="logPath" height="min(60vh, 560px)" empty-text="No output from this instance yet." />
        <div v-else class="empty">
          <Icon name="terminal" :size="28" />
          <p v-if="!stack.running">The stack is not running; the live log is unavailable.</p>
          <p v-else>Connecting…</p>
        </div>
      </section>

      <BuildLogModal v-if="stack.id" :open="showBuildLog" :stack-id="stack.id" :service="svc.name" @close="showBuildLog = false" />
      <KeptLogsModal v-if="stack.id" :open="showKeptLogs" :stack-id="stack.id" :service="svc.name" @close="showKeptLogs = false" />
    </template>

    <div v-else class="card">
      <div class="empty">
        <Icon name="search" :size="32" />
        <h2>No service “{{ name }}”</h2>
        <p v-if="stack.view">This stack has no service of that name.</p>
        <p v-else>No state seen for this stack yet.</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.back {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 13px;
  color: var(--text-muted);
  align-self: flex-start;
}
.head-left {
  gap: 12px;
  align-items: flex-start;
  min-width: 0;
}
.kind {
  width: 40px;
  height: 40px;
  border-radius: 11px;
  display: grid;
  place-items: center;
  background: var(--surface-3);
  color: var(--text-2);
  flex: none;
}
.subtitle a {
  display: inline-flex;
  align-items: center;
  gap: 3px;
}
.live-line {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 14px;
  border-radius: var(--radius);
  background: var(--ok-soft);
  color: var(--ok-text);
  font-size: 13px;
  border: 1px solid color-mix(in srgb, var(--ok) 30%, transparent);
}
.cols {
  display: grid;
  grid-template-columns: minmax(0, 1.1fr) minmax(0, 1fr);
  gap: 16px;
  align-items: start;
}
@media (max-width: 900px) {
  .cols {
    grid-template-columns: minmax(0, 1fr);
  }
}
.side {
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
}
.proxy-flow {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 14px;
}
.node {
  display: flex;
  flex-direction: column;
  padding: 6px 10px;
  border-radius: 8px;
  background: var(--surface-2);
  border: 1px solid var(--border);
  min-width: 80px;
}
.node.dim {
  opacity: 0.6;
}
.node.pending {
  border-style: dashed;
  border-color: color-mix(in srgb, var(--accent) 50%, transparent);
  background: var(--accent-soft);
}
.nl {
  font-size: 10.5px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-muted);
  font-weight: 600;
}
.arrow {
  color: var(--text-faint);
  font-size: 18px;
}
.err {
  color: var(--danger-text);
  font-family: var(--font-mono);
  font-size: 12.5px;
}
.res {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 18px;
}
@media (max-width: 520px) {
  .res {
    grid-template-columns: 1fr;
  }
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}
.mh {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
}
.ml {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  color: var(--text-muted);
  font-weight: 600;
}
.mv {
  font-size: 18px;
  font-weight: 650;
}
.mf {
  font-size: 11.5px;
}
.log-card {
  overflow: hidden;
}
</style>
