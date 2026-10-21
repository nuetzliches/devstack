<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import type { CommandView, ContainerView, ServiceView } from '@/api/types';
import { containerBucket, parseComposeMember, statusBucket } from '@/lib/status';
import { useHub } from '@/stores/hub';
import { useStack } from '@/stores/stack';
import ServiceRow from '@/components/ServiceRow.vue';
import ContainerRow from '@/components/ContainerRow.vue';
import CommandRow from '@/components/CommandRow.vue';
import Icon from '@/components/icons/Icon.vue';
import Sparkline from '@/components/Sparkline.vue';
import RowUsage from '@/components/RowUsage.vue';
import { bytes, percent } from '@/lib/format';
import { persist } from '@/lib/persist';
import { usePins } from '@/lib/pins';

const stack = useStack();
const hub = useHub();

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

/** A stat tile picked as filter: only those members show (click again: all). */
type Filter = 'healthy' | 'starting' | 'failed' | 'idle' | 'pending';
const filter = ref<Filter | null>(null);
function toggle(f: Filter) {
  filter.value = filter.value === f ? null : f;
}
function matches(m: Member, f: Filter): boolean {
  if (m.kind === 'service') return f === 'pending' ? !!m.service.candidate : statusBucket(m.service.status) === f;
  if (m.kind === 'container') return f !== 'pending' && !!m.container && containerBucket(m.container) === f;
  return false;
}
const pins = usePins();
/** Pinned members first, in their own section (they stay in their group too). */
const withPinned = computed(() => {
  const keys = pins.list(stack.id);
  if (!keys.length) return cards.value;
  const all = new Map<string, Member>();
  for (const c of cards.value) for (const m of c.members) all.set(m.key, m);
  const members = keys.map((k) => all.get(k)).filter((m): m is Member => !!m);
  return members.length ? [{ key: '__pinned', label: 'Pinned', members }, ...cards.value] : cards.value;
});
const shown = computed(() => {
  const f = filter.value;
  if (!f) return withPinned.value;
  return withPinned.value.map((c) => ({ ...c, members: c.members.filter((m) => matches(m, f)) })).filter((c) => c.members.length);
});

/** Sections folded away, per stack, kept in this browser; only their head folds them. */
const folded = ref<Record<string, boolean>>({});
watch(
  () => stack.id,
  (id) => (folded.value = (id && persist.get<Record<string, boolean>>(`folded.${id}`)) || {}),
  { immediate: true },
);
function fold(key: string) {
  folded.value[key] = !folded.value[key];
  if (stack.id) persist.set(`folded.${stack.id}`, folded.value);
}

/** What a group costs now (the usage report sums it). */
function groupLoad(key: string) {
  return stack.usage?.groups.find((g) => g.name === key) ?? null;
}
/** The shown tree in total: its services, containers and builds. */
const total = computed(() => stack.usage?.lanes.find((l) => l.name === (stack.lane ?? 'main')) ?? stack.usage?.lanes[0] ?? null);
const totalMem = computed(() => (stack.history.get(`lane:${total.value?.name ?? 'main'}`) ?? []).map((p) => p.memory));
const filterLabel: Record<Filter, string> = { healthy: 'healthy', starting: 'starting', failed: 'failing', idle: 'stopped or done', pending: 'not live yet' };

const counts = computed(() => {
  const c = { healthy: 0, starting: 0, failed: 0, idle: 0 };
  const v = view.value;
  if (!v) return c;
  for (const s of v.services) c[statusBucket(s.status)]++;
  for (const g of v.infra) for (const x of g.containers) c[containerBucket(x)]++;
  return c;
});

const hubAway = computed(() => !hub.reachable);
const groupActions: { action: 'start' | 'stop' | 'restart'; title: string; icon: 'play' | 'stop' | 'restart' }[] = [  { action: 'start', title: 'Start every service of', icon: 'play' },  { action: 'restart', title: 'Rebuild and restart every service and container of', icon: 'restart' },  { action: 'stop', title: 'Stop every service of', icon: 'stop' },];
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
</script>

<template>
  <div class="page page-wide">
    <template v-if="view">
        <div class="stats stale-dim" role="group" aria-label="Filter the overview by state">
          <button
            type="button"
            class="stat ok"
            :class="{ active: filter === 'healthy' }"
            :aria-pressed="filter === 'healthy'"
            :title="filter === 'healthy' ? 'Show everything again' : 'Show only what is healthy'"
            data-filter="healthy"
            @click="toggle('healthy')"
          >
            <span class="v tabular">{{ counts.healthy }}</span><span class="l">healthy</span>
          </button>
          <button
            type="button"
            class="stat accent"
            :class="{ active: filter === 'starting' }"
            :aria-pressed="filter === 'starting'"
            :title="filter === 'starting' ? 'Show everything again' : 'Show only what is starting'"
            data-filter="starting"
            @click="toggle('starting')"
          >
            <span class="v tabular">{{ counts.starting }}</span><span class="l">starting</span>
          </button>
          <button
            type="button"
            class="stat danger"
            :class="{ active: filter === 'failed', zero: !counts.failed }"
            :aria-pressed="filter === 'failed'"
            :title="filter === 'failed' ? 'Show everything again' : 'Show only what is failing'"
            data-filter="failed"
            @click="toggle('failed')"
          >
            <span class="v tabular">{{ counts.failed }}</span><span class="l">failing</span>
          </button>
          <button
            type="button"
            class="stat neutral"
            :class="{ active: filter === 'idle' }"
            :aria-pressed="filter === 'idle'"
            :title="filter === 'idle' ? 'Show everything again' : 'Show only what is stopped or done'"
            data-filter="idle"
            @click="toggle('idle')"
          >
            <span class="v tabular">{{ counts.idle }}</span><span class="l">stopped or done</span>
          </button>
          <button
            type="button"
            class="stat warn"
            :class="{ active: filter === 'pending', zero: !stack.pending.length }"
            :aria-pressed="filter === 'pending'"
            :title="filter === 'pending' ? 'Show everything again' : 'Show only what is not live yet'"
            data-filter="pending"
            data-testid="pending-count"
            @click="toggle('pending')"
          >
            <span class="v tabular">{{ stack.pending.length }}</span><span class="l">not live yet</span>
          </button>
          <RouterLink class="stat usage" :to="{ name: 'usage', params: { stack: stack.id ?? '' } }" title="CPU and memory of this tree now, memory over the last 10 minutes; click for the details (builds, disk)" data-testid="usage-total">
            <span class="v tabular">{{ bytes(total?.memory) }}</span>
            <span class="l">memory · CPU {{ percent(total?.cpu) }}</span>
            <Sparkline class="spark" :values="totalMem" :width="90" :height="22" :min-max="64 * 1024 * 1024" :zero-based="false" color="var(--ok)" />
          </RouterLink>
        </div>

      <p v-if="filter" class="filter-note muted" data-testid="overview-filter">
        Showing only what is {{ filterLabel[filter] }}<template v-if="!shown.length">: nothing right now</template>.
        <button type="button" class="link-btn" @click="filter = null">Show everything</button>
      </p>
      <div class="md">
      <div class="list stale-dim" data-testid="member-list">
        <section v-for="card in shown" :key="card.key" class="card section" :class="{ folded: folded[card.key] }" :data-testid="`group-${card.key}`">
          <header class="card-header section-head">
            <button type="button" class="fold" :aria-expanded="!folded[card.key]" :title="folded[card.key] ? 'Show' : 'Fold away'" data-testid="section-fold" @click="fold(card.key)">
              <Icon :name="folded[card.key] ? 'chevronRight' : 'chevronDown'" :size="14" />
              <Icon v-if="card.key === '__pinned'" name="pin" :size="13" />
              <h2>{{ card.label }}</h2>
              <span class="count">{{ card.members.length }}</span>
            </button>
            <RowUsage :series-key="`grp:${card.key}`" :load="groupLoad(card.key)" />
            <span class="grow" />
            <div v-if="!stack.readOnly && card.key !== '__other' && card.key !== '__pinned' && card.members.some((m) => m.kind === 'service')" class="group-actions" role="group" :aria-label="`Actions for ${card.label}`">
              <button
                v-for="a in groupActions"
                :key="a.action"
                type="button"
                class="btn btn-ghost btn-sm btn-icon"
                :class="{ 'btn-stop': a.action === 'stop' }"
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
          <ul v-show="!folded[card.key]" class="members">
            <template v-for="m in card.members" :key="m.key">
              <ServiceRow v-if="m.kind === 'service'" :service="m.service" />
              <ContainerRow v-else-if="m.kind === 'container'" :group="m.group" :service="m.name" :container="m.container" />
              <CommandRow v-else-if="m.kind === 'command'" :command="m.command" />
              <li v-else class="unknown"><Icon name="info" :size="14" /> {{ m.key }} <span class="faint">(not in this stack)</span></li>
            </template>
          </ul>
        </section>
      </div>
      <div class="detail">
        <RouterView />
      </div>
      </div>
    </template>

    <div v-else class="card">
      <div class="empty">
        <Icon name="layers" :size="36" />
        <h2>No state seen for this stack yet</h2>
        <p v-if="stack.entry && !stack.running">Start it to see its services, infrastructure and commands.</p>
        <p v-else-if="stack.running">Waiting for the first snapshot…</p>
        <button v-if="stack.entry && !stack.running && !stack.readOnly" type="button" class="btn btn-primary" :disabled="starting" @click="stack.startStack()">
          <span v-if="starting" class="spinner" /><Icon v-else name="play" /> Start stack
        </button>
      </div>
    </div>

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
  align-items: flex-start;
  font: inherit;
  color: inherit;
  text-align: left;
  cursor: pointer;
  transition:
    border-color 0.12s,
    box-shadow 0.12s;
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
.stat:hover {
  border-color: color-mix(in srgb, var(--tone) 55%, var(--border));
}
.stat.active {
  border-color: var(--tone);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--tone) 35%, transparent);
  background: color-mix(in srgb, var(--tone) 8%, var(--surface-2));
}
.filter-note {
  font-size: 13px;
  margin: 0;
}
.link-btn {
  border: 0;
  background: none;
  padding: 0;
  color: var(--accent);
  font: inherit;
  cursor: pointer;
  text-decoration: underline;
}
.md {
  display: grid;
  grid-template-columns: minmax(380px, 40%) minmax(0, 1fr);
  gap: var(--space-4);
  align-items: start;
}
/* The list stays in view while the detail scrolls. */
.list {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  position: sticky;
  top: calc(var(--topbar-h) + var(--banners-h) + 12px);
  max-height: calc(100vh - var(--topbar-h) - var(--banners-h) - 24px);
  overflow-y: auto;
  padding-right: 2px;
}
.detail {
  min-width: 0;
}
@media (max-width: 1100px) {
  .md {
    grid-template-columns: minmax(0, 1fr);
  }
  .list {
    position: static;
    max-height: none;
  }
}
.stats {
  grid-template-columns: repeat(6, minmax(0, 1fr));
}
.stat.usage {
  --tone: var(--ok);
  text-decoration: none;
  position: relative;
}
.stat.usage .spark {
  position: absolute;
  right: 10px;
  top: 10px;
  opacity: 0.8;
}
.section-head {
  gap: 8px;
}
.fold {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  padding: 0;
  cursor: pointer;
  flex: 1;
  min-width: 0;
  text-align: left;
}
.fold h2 {
  margin: 0;
}
.section.folded .section-head {
  border-bottom: 0;
}
</style>
