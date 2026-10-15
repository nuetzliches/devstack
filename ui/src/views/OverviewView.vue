<script setup lang="ts">
import { computed, ref } from 'vue';
import type { CommandView, ContainerView, ServiceView } from '@/api/types';
import { ago, duration, useNow } from '@/lib/format';
import { containerBucket, parseComposeMember, statusBucket } from '@/lib/status';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import ServiceRow from '@/components/ServiceRow.vue';
import ContainerRow from '@/components/ContainerRow.vue';
import CommandRow from '@/components/CommandRow.vue';
import CandidatePanel from '@/components/CandidatePanel.vue';
import BuildLogModal from '@/components/BuildLogModal.vue';
import Icon from '@/components/icons/Icon.vue';

const stack = useStack();
const hub = useHub();
const now = useNow();
const buildLogFor = ref<string | null>(null);

type Member =
  | { kind: 'service'; key: string; service: ServiceView }
  | { kind: 'container'; key: string; group: string; name: string; container: ContainerView | null }
  | { kind: 'command'; key: string; command: CommandView }
  | { kind: 'unknown'; key: string };

const view = computed(() => stack.view);

function resolve(m: string): Member {
  const v = view.value!;
  const compose = parseComposeMember(m);
  if (compose) {
    const [group, name] = compose;
    const c = v.infra.find((g) => g.group === group)?.containers.find((x) => x.service === name) ?? null;
    return { kind: 'container', key: m, group, name, container: c };
  }
  if (m.startsWith('command.')) {
    const c = v.commands.find((x) => x.name === m.slice(8));
    return c ? { kind: 'command', key: m, command: c } : { kind: 'unknown', key: m };
  }
  const s = v.services.find((x) => x.name === m);
  return s ? { kind: 'service', key: m, service: s } : { kind: 'unknown', key: m };
}

const cards = computed(() => {
  const v = view.value;
  if (!v) return [];
  const groups = [...v.groups].sort((a, b) => (a.order ?? Number.MAX_SAFE_INTEGER) - (b.order ?? Number.MAX_SAFE_INTEGER) || a.label.localeCompare(b.label));
  const seen = new Set<string>();
  const out = groups.map((g) => {
    g.members.forEach((m) => seen.add(m));
    return { key: g.name, label: g.label || g.name, members: g.members.map(resolve) };
  });
  const other: Member[] = [];
  for (const s of v.services) if (!seen.has(s.name)) other.push({ kind: 'service', key: s.name, service: s });
  for (const g of v.infra)
    for (const c of g.containers) {
      const k = `compose.${g.group}.${c.service}`;
      if (!seen.has(k)) other.push({ kind: 'container', key: k, group: g.group, name: c.service, container: c });
    }
  for (const c of v.commands) if (!seen.has(`command.${c.name}`)) other.push({ kind: 'command', key: `command.${c.name}`, command: c });
  if (other.length) out.push({ key: '__other', label: 'Other', members: other });
  return out;
});

const counts = computed(() => {
  const c = { healthy: 0, starting: 0, failed: 0, idle: 0 };
  const v = view.value;
  if (!v) return c;
  for (const s of v.services) c[statusBucket(s.status)]++;
  for (const g of v.infra) for (const x of g.containers) c[containerBucket(x)]++;
  return c;
});

const attention = computed(() => stack.pending.filter((s) => s.candidate && (s.candidate.state === 'held' || s.candidate.state === 'failed')));
const inFlight = computed(() => stack.pending.length - attention.value.length);
const infraErrors = computed(() => view.value?.infra.filter((g) => g.error) ?? []);
const daemon = computed(() => stack.entry?.daemon ?? null);
const hubAway = computed(() => !hub.reachable);
const groupActions: { action: 'start' | 'stop' | 'restart'; title: string; icon: 'play' | 'stop' | 'restart' }[] = [  { action: 'start', title: 'Start every service of', icon: 'play' },  { action: 'restart', title: 'Rebuild and restart every service and container of', icon: 'restart' },  { action: 'stop', title: 'Stop every service of', icon: 'stop' },];
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
</script>

<template>
  <div class="page">
    <template v-if="view">
      <section class="summary card" data-testid="stack-summary">
        <div class="sum-main">
          <div class="grow">
            <div class="row wrap title-row">
              <h1>{{ view.name }}</h1>
              <span v-if="hubAway" class="pill" :title="'The hub is not reachable'"><span class="dot" />last seen {{ ago(stack.viewAt ?? hub.lastContact, now) }}</span>
              <span v-else class="pill" :class="stack.running ? 'ok' : ''"><span class="dot" />{{ stack.running ? 'running' : 'offline' }}</span>
              <span v-if="daemon" class="tag" title="daemon version">devstack {{ daemon.version }}</span>
            </div>
            <div class="repo truncate" :title="view.repo">{{ view.repo }}</div>
            <div v-if="daemon" class="daemon muted">daemon pid {{ daemon.pid }} · port {{ daemon.port }} · up {{ duration(now - daemon.started) }}</div>
          </div>
          <div v-if="!hubAway" class="btn-group">
            <button v-if="!stack.running" type="button" class="btn btn-primary" :disabled="starting || !stack.entry" data-testid="action-stack-start" @click="stack.startStack()">
              <span v-if="starting" class="spinner" /><Icon v-else name="play" /> Start stack
            </button>
            <button v-else type="button" class="btn" :disabled="!!stack.busy.stack" data-testid="action-stack-stop" @click="stack.stopStack()">
              <span v-if="stack.busy.stack" class="spinner" /><Icon v-else name="power" /> Stop stack
            </button>
          </div>
        </div>
        <div class="stats stale-dim">
          <div class="stat ok">
            <span class="v tabular">{{ counts.healthy }}</span><span class="l">healthy</span>
          </div>
          <div class="stat accent">
            <span class="v tabular">{{ counts.starting }}</span><span class="l">starting</span>
          </div>
          <div class="stat danger" :class="{ zero: !counts.failed }">
            <span class="v tabular">{{ counts.failed }}</span><span class="l">failing</span>
          </div>
          <div class="stat neutral">
            <span class="v tabular">{{ counts.idle }}</span><span class="l">stopped or done</span>
          </div>
          <div class="stat warn" :class="{ zero: !stack.pending.length }" data-testid="pending-count">
            <span class="v tabular">{{ stack.pending.length }}</span><span class="l">not live yet</span>
          </div>
        </div>
      </section>

      <div v-for="g in infraErrors" :key="g.group" class="banner danger">
        <Icon name="alert" :size="18" />
        <span class="grow"><strong>Compose group {{ g.group }}:</strong> {{ g.error }}</span>
      </div>

      <section v-if="attention.length" class="attention stale-dim" aria-label="Needs attention">
        <h2 class="section-title">Needs attention</h2>
        <div v-for="s in attention" :key="s.name" class="att-item">
          <RouterLink :to="{ name: 'service', params: { stack: stack.id ?? '', name: s.name } }" class="att-name">{{ s.name }} <Icon name="chevronRight" :size="14" /></RouterLink>
          <CandidatePanel :service="s" @build-log="buildLogFor = s.name" />
        </div>
      </section>
      <p v-if="inFlight" class="inflight muted"><span class="spinner" /> {{ inFlight }} change{{ inFlight === 1 ? '' : 's' }} on the way; the running instances keep serving until the new builds are healthy.</p>

      <div class="grid-cards stale-dim">
        <section v-for="card in cards" :key="card.key" class="card" :data-testid="`group-${card.key}`">
          <header class="card-header">
            <h2>{{ card.label }}</h2>
            <span class="count">{{ card.members.length }}</span>
            <span class="grow" />
            <div v-if="card.key !== '__other' && card.members.some((m) => m.kind === 'service')" class="group-actions" role="group" :aria-label="`Actions for ${card.label}`">
              <button
                v-for="a in groupActions"
                :key="a.action"
                type="button"
                class="btn btn-ghost btn-sm btn-icon"
                :title="`${a.title} ${card.label}`"
                :aria-label="`${a.title} ${card.label}`"
                :disabled="!!stack.busy[`grp:${card.key}`] || stack.stale || hubAway"
                :data-testid="`group-${a.action}`"
                @click="stack.groupAction(card.key, card.label, a.action)"
              >
                <span v-if="stack.busy[`grp:${card.key}`] === a.action" class="spinner" /><Icon v-else :name="a.icon" />
              </button>
            </div>
          </header>
          <ul class="members">
            <template v-for="m in card.members" :key="m.key">
              <ServiceRow v-if="m.kind === 'service'" :service="m.service" />
              <ContainerRow v-else-if="m.kind === 'container'" :group="m.group" :service="m.name" :container="m.container" />
              <CommandRow v-else-if="m.kind === 'command'" :command="m.command" />
              <li v-else class="unknown"><Icon name="info" :size="14" /> {{ m.key }} <span class="faint">(not in this stack)</span></li>
            </template>
          </ul>
        </section>
      </div>
    </template>

    <div v-else class="card">
      <div class="empty">
        <Icon name="layers" :size="36" />
        <h2>No state seen for this stack yet</h2>
        <p v-if="stack.entry && !stack.running">Start it to see its services, infrastructure and commands.</p>
        <p v-else-if="stack.running">Waiting for the first snapshot…</p>
        <button v-if="stack.entry && !stack.running" type="button" class="btn btn-primary" :disabled="starting" @click="stack.startStack()">
          <span v-if="starting" class="spinner" /><Icon v-else name="play" /> Start stack
        </button>
      </div>
    </div>

    <BuildLogModal v-if="buildLogFor && stack.id" :open="!!buildLogFor" :stack-id="stack.id" :service="buildLogFor" @close="buildLogFor = null" />
  </div>
</template>

<style scoped>
.group-actions {
  display: flex;
  gap: 2px;
  margin: -4px -8px -4px 0;
}
.summary {
  padding: 18px 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.sum-main {
  display: flex;
  gap: 16px;
  align-items: flex-start;
  flex-wrap: wrap;
}
.title-row {
  gap: 10px;
}
.repo {
  font-family: var(--font-mono);
  font-size: 12.5px;
  color: var(--text-muted);
  margin-top: 4px;
}
.daemon {
  font-size: 12px;
  margin-top: 2px;
}
.stats {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 10px;
}
@media (max-width: 640px) {
  .stats {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}
.stat {
  --tone: var(--text-muted);
  display: flex;
  flex-direction: column;
  padding: 10px 12px;
  border-radius: var(--radius);
  background: var(--surface-2);
  border: 1px solid var(--border);
  position: relative;
  overflow: hidden;
}
.stat::before {
  content: '';
  position: absolute;
  left: 0;
  top: 0;
  bottom: 0;
  width: 3px;
  background: var(--tone);
}
.stat .v {
  font-size: 22px;
  font-weight: 650;
  line-height: 1.1;
}
.stat .l {
  font-size: 12px;
  color: var(--text-muted);
}
.stat.ok {
  --tone: var(--ok);
}
.stat.accent {
  --tone: var(--accent);
}
.stat.danger {
  --tone: var(--danger);
}
.stat.danger:not(.zero) .v {
  color: var(--danger-text);
}
.stat.warn {
  --tone: var(--warn);
}
.stat.warn:not(.zero) .v {
  color: var(--warn-text);
}
.stat.neutral {
  --tone: var(--border-strong);
}
.stat.zero::before {
  background: var(--border-strong);
}
.attention {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.att-item {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.att-name {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  font-weight: 650;
  color: var(--text);
  align-self: flex-start;
}
.inflight {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}
.members {
  list-style: none;
  margin: 0;
  padding: 0;
}
.unknown {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 10px 16px;
  color: var(--text-muted);
  font-size: 13px;
}
</style>
