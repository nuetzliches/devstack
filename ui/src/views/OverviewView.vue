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
import RowUsage from '@/components/RowUsage.vue';
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

/** A state badge picked as filter: only those members show (click again: all). */
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
const filterLabel: Record<Filter, string> = { healthy: 'healthy', starting: 'starting', failed: 'failing', idle: 'stopped or done', pending: 'not live yet' };

const counts = computed(() => {
  const c = { healthy: 0, starting: 0, failed: 0, idle: 0 };
  const v = view.value;
  if (!v) return c;
  for (const s of v.services) c[statusBucket(s.status)]++;
  for (const g of v.infra) for (const x of g.containers) c[containerBucket(x)]++;
  return c;
});
const badges = computed<{ f: Filter; n: number; tone: string }[]>(() => [
  { f: 'healthy', n: counts.value.healthy, tone: 'ok' },
  { f: 'starting', n: counts.value.starting, tone: 'accent' },
  { f: 'failed', n: counts.value.failed, tone: 'danger' },
  { f: 'idle', n: counts.value.idle, tone: 'neutral' },
  { f: 'pending', n: stack.pending.length, tone: 'warn' },
]);

/** A state nothing is in is left out (healthy stays, as the anchor), unless it is the filter. */
const shownBadges = computed(() => badges.value.filter((b) => b.n || b.f === 'healthy' || filter.value === b.f));

const hubAway = computed(() => !hub.reachable);
const groupActions: { action: 'start' | 'stop' | 'restart'; title: string; icon: 'play' | 'stop' | 'restart' }[] = [  { action: 'start', title: 'Start every service of', icon: 'play' },  { action: 'restart', title: 'Rebuild and restart every service and container of', icon: 'restart' },  { action: 'stop', title: 'Stop every service of', icon: 'stop' },];
const starting = computed(() => !!(stack.id && hub.starting[stack.id]));
</script>

<template>
  <div class="page page-wide">
    <template v-if="view">
      <div class="md">
        <div class="side">
          <div class="filters stale-dim" role="group" aria-label="Filter the overview by state">
            <button
              v-for="b in shownBadges"
              :key="b.f"
              type="button"
              class="badge"
              :class="[b.tone, { active: filter === b.f, zero: !b.n }]"
              :aria-pressed="filter === b.f"
              :title="filter === b.f ? 'Show everything again' : `Show only what is ${filterLabel[b.f]}`"
              :data-filter="b.f"
              :data-testid="b.f === 'pending' ? 'pending-count' : undefined"
              @click="toggle(b.f)"
            >
              <span class="dot" /><span class="v tabular">{{ b.n }}</span> {{ filterLabel[b.f] }}<Icon v-if="filter === b.f" name="x" :size="12" />
            </button>
            <button v-if="filter" type="button" class="btn btn-ghost btn-xs" data-testid="overview-filter-reset" @click="filter = null">
              <Icon name="rollback" :size="12" /> Reset filter
            </button>
          </div>
          <p v-if="filter" class="filter-note muted" data-testid="overview-filter">
            Showing only what is {{ filterLabel[filter] }}<template v-if="!shown.length">: nothing right now</template>.
          </p>
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
.filter-note {
  font-size: 13px;
  margin: 0;
}
.md {
  display: grid;
  grid-template-columns: minmax(440px, 46%) minmax(0, 1fr);
  gap: var(--space-4);
  align-items: start;
}
/* The list, under its filters, stays in view while the detail scrolls. */
.side {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  position: sticky;
  top: calc(var(--topbar-h) + var(--banners-h) + 12px);
  max-height: calc(100vh - var(--topbar-h) - var(--banners-h) - 24px);
  min-width: 0;
}
.list {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  overflow-y: auto;
  min-height: 0;
  padding-right: 2px;
}
.filters {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.badge {
  --tone: var(--text-muted);
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--text-muted);
  font: inherit;
  font-size: 12px;
  line-height: 18px;
  cursor: pointer;
  transition:
    border-color 0.12s,
    background 0.12s;
}
.badge .dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--tone);
}
.badge .v {
  font-weight: 650;
  color: var(--text);
}
.badge.ok {
  --tone: var(--ok);
}
.badge.accent {
  --tone: var(--accent);
}
.badge.danger {
  --tone: var(--danger);
}
.badge.danger:not(.zero) .v {
  color: var(--danger-text);
}
.badge.warn {
  --tone: var(--warn);
}
.badge.warn:not(.zero) .v {
  color: var(--warn-text);
}
.badge.neutral {
  --tone: var(--border-strong);
}
.badge.zero .dot {
  background: var(--border-strong);
}
.badge:hover {
  border-color: color-mix(in srgb, var(--tone) 55%, var(--border));
}
/* The filter: in the accent, whatever its state's colour (a neutral one barely shows). */
.badge.active {
  border-color: var(--accent);
  background: var(--accent-soft);
  color: var(--accent-text);
  box-shadow: 0 0 0 1px var(--accent);
}
.badge.active .v {
  color: var(--accent-text);
}
.detail {
  min-width: 0;
}
@media (max-width: 1100px) {
  .md {
    grid-template-columns: minmax(0, 1fr);
  }
  .side {
    position: static;
    max-height: none;
  }
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
