<script setup lang="ts">
import { computed, ref } from 'vue';
import type { Client, JournalEntry } from '@/api/types';
import { clock, shortBuild } from '@/lib/format';
import { clientLabel, journalLabel, journalService, viaText } from '@/lib/journal';
import { candidateLook, candidateSummary, rebuildLook, rebuildSummary, statusLook, type Tone } from '@/lib/status';
import { useStack, type TimelineEvent } from '@/stores/stack';
import Icon from '@/components/icons/Icon.vue';
import type { IconName } from '@/components/icons/icons';

type Type = TimelineEvent['type'] | 'journal';
const stack = useStack();
const types: { v: Type; label: string }[] = [
  { v: 'journal', label: 'Changed by' },
  { v: 'service', label: 'Service' },
  { v: 'candidate', label: 'Candidate' },
  { v: 'devserver_rebuild', label: 'Rebuild' },
  { v: 'infra', label: 'Infra' },
  { v: 'command', label: 'Command' },
  { v: 'config', label: 'Stack file' },
  { v: 'watcher', label: 'Watcher' },
  { v: 'notice', label: 'Notice' },
  { v: 'snapshot', label: 'Connect' },
];
const enabled = ref<Record<Type, boolean>>({ journal: true, service: true, candidate: true, devserver_rebuild: true, infra: true, command: true, config: true, watcher: true, notice: true, snapshot: true });
const subject = ref('');
/** Changes through this client only; events stay. */
const client = ref<Client | ''>('');
/** Changes of this lane only (`main`: the main tree's); events stay. */
const laneOf = ref('');

/** A row of the timeline: an event of the stream, or a change the journal recorded (ADR-0067). */
type Row = { kind: 'event'; at: number; key: string; e: TimelineEvent } | { kind: 'journal'; at: number; key: string; j: JournalEntry };

function subjectOf(r: Row): string {
  if (r.kind === 'journal') return journalService(r.j) ?? '';
  const e = r.e;
  switch (e.type) {
    case 'service':
    case 'candidate':
    case 'devserver_rebuild':
      return e.service;
    case 'infra':
      return e.group;
    case 'command':
      return e.command;
    case 'config':
    case 'watcher':
    case 'notice':
    case 'snapshot':
      return '';
  }
}

const rows = computed<Row[]>(() => {
  const all: Row[] = [
    ...stack.events.map((e) => ({ kind: 'event' as const, at: e.at, key: `e${e.seq}-${e.at}`, e })),
    ...stack.journal.map((j, i) => ({ kind: 'journal' as const, at: j.at, key: `j${j.at}-${i}`, j })),
  ];
  // Newest first; at the same moment, what was caused above the change that caused it.
  return all.sort((a, b) => b.at - a.at || (a.kind === b.kind ? 0 : a.kind === 'event' ? -1 : 1));
});

const subjects = computed(() => {
  const set = new Set<string>();
  for (const s of stack.view?.services ?? []) set.add(s.name);
  for (const r of rows.value) {
    const s = subjectOf(r);
    if (s) set.add(s);
  }
  return [...set].sort();
});
const clients = computed(() => [...new Set(stack.journal.map((j) => j.via))].sort());
const lanes = computed(() => [...new Set(stack.journal.map((j) => j.lane).filter((l): l is string => !!l))].sort());

const shown = computed(() =>
  rows.value.filter((r) => {
    if (subject.value && subjectOf(r) !== subject.value) return false;
    if (r.kind === 'event') return enabled.value[r.e.type];
    if (!enabled.value.journal) return false;
    if (client.value && r.j.via !== client.value) return false;
    if (laneOf.value && (r.j.lane ?? 'main') !== laneOf.value) return false;
    return true;
  }),
);

type Desc = { icon: IconName; tone: Tone; title: string; text: string; detail?: string | null };

function describeJournal(j: JournalEntry): Desc {
  const by = viaText(j.via);
  const where = j.lane ? ` · lane ${j.lane}` : '';
  if (j.status >= 400) return { icon: 'xCircle', tone: 'danger', title: journalLabel(j), text: `refused (${j.status}) · ${by}${where}`, detail: j.error ?? null };
  return { icon: j.via === 'auto' ? 'zap' : 'history', tone: 'accent', title: journalLabel(j), text: `${by}${where}` };
}

function describe(r: Row): Desc {
  if (r.kind === 'journal') return describeJournal(r.j);
  const e = r.e;
  switch (e.type) {
    case 'service': {
      const l = statusLook(e.status);
      return { icon: 'server', tone: l.tone, title: e.service, text: `${l.label}${e.build_id ? ` · build ${shortBuild(e.build_id)}` : ''}`, detail: l.detail };
    }
    case 'candidate': {
      if (!e.candidate) return { icon: 'checkCircle', tone: 'ok', title: e.service, text: 'Candidate done; the running build is current' };
      const l = candidateLook(e.candidate);
      return {
        icon: l.icon,
        tone: l.tone,
        title: e.service,
        text: `${candidateSummary(e.candidate)}${e.candidate.build_id ? ` · ${shortBuild(e.candidate.build_id)}` : ''}`,
        detail: e.candidate.reason,
      };
    }
    case 'devserver_rebuild': {
      if (!e.rebuild) return { icon: 'checkCircle', tone: 'neutral', title: e.service, text: 'Rebuild shown no longer' };
      const l = rebuildLook(e.rebuild);
      return { icon: l.icon, tone: l.tone, title: e.service, text: `${rebuildSummary(e.rebuild)} · ${e.rebuild.tool}`, detail: e.rebuild.errors?.join('\n') ?? null };
    }
    case 'infra':
      return { icon: 'box', tone: 'info', title: e.group, text: e.message };
    case 'command':
      return { icon: 'terminal', tone: e.status === 'failed' ? 'danger' : e.status === 'succeeded' ? 'ok' : 'accent', title: e.command, text: `run #${e.run} ${e.status}` };
    case 'config':
      return e.changed
        ? { icon: 'file', tone: 'warn', title: 'Stack file', text: 'Changed since the daemon started; applies with the next start' }
        : { icon: 'file', tone: 'ok', title: 'Stack file', text: 'Back to what the daemon runs with' };
    case 'watcher':
      return e.error
        ? { icon: 'alert', tone: 'warn', title: 'File watcher', text: 'Edits may not be seen', detail: e.error }
        : { icon: 'checkCircle', tone: 'ok', title: 'File watcher', text: 'Works again' };
    case 'notice':
      return { icon: 'alert', tone: e.level === 'error' ? 'danger' : 'warn', title: 'Notice', text: e.text };
    case 'snapshot':
      return { icon: 'activity', tone: 'neutral', title: 'Connected', text: `snapshot of ${e.stack.services.length} services` };
  }
}
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>Activity</h1>
        <p class="subtitle">What changed the stack and through which client (its journal, kept across restarts), and the live events since this page connected; newest first.</p>
      </div>
    </header>

    <section class="card">
      <header class="card-header filters">
        <div class="btn-group" role="group" aria-label="Event types">
          <button v-for="t in types" :key="t.v" type="button" class="chip-toggle" :aria-pressed="enabled[t.v]" :data-testid="`events-filter-${t.v}`" @click="enabled[t.v] = !enabled[t.v]">
            {{ t.label }}
          </button>
        </div>
        <span class="grow" />
        <template v-if="clients.length">
          <label class="sr-only" for="ev-client">Filter changes by client</label>
          <select id="ev-client" v-model="client" class="input input-sm" data-testid="events-client">
            <option value="">All clients</option>
            <option v-for="c in clients" :key="c" :value="c">{{ clientLabel[c] ?? c }}</option>
          </select>
        </template>
        <template v-if="lanes.length">
          <label class="sr-only" for="ev-lane">Filter changes by lane</label>
          <select id="ev-lane" v-model="laneOf" class="input input-sm" data-testid="events-lane">
            <option value="">All lanes</option>
            <option value="main">Main tree</option>
            <option v-for="l in lanes" :key="l" :value="l">{{ l }}</option>
          </select>
        </template>
        <label class="sr-only" for="ev-subject">Filter by service</label>
        <select id="ev-subject" v-model="subject" class="input input-sm" data-testid="events-subject">
          <option value="">All services</option>
          <option v-for="s in subjects" :key="s" :value="s">{{ s }}</option>
        </select>
      </header>
      <ol v-if="shown.length" class="timeline stale-dim" data-testid="events-list" aria-live="polite">
        <li v-for="r in shown" :key="r.key" class="ev" :class="`tone-${describe(r).tone}`" :data-type="r.kind === 'journal' ? 'journal' : r.e.type">
          <time class="t tabular" :datetime="new Date(r.at).toISOString()">{{ clock(r.at) }}</time>
          <span class="ic"><Icon :name="describe(r).icon" :size="14" /></span>
          <div class="grow body">
            <div class="line1">
              <span class="ty">{{ r.kind === 'journal' ? 'changed' : r.e.type }}</span>
              <RouterLink v-if="r.kind === 'event' && (r.e.type === 'service' || r.e.type === 'candidate') && stack.svc(r.e.service)" :to="{ name: 'service', params: { stack: stack.id ?? '', name: r.e.service } }" class="subj">{{ describe(r).title }}</RouterLink>
              <span v-else class="subj">{{ describe(r).title }}</span>
              <span class="txt">{{ describe(r).text }}</span>
            </div>
            <pre v-if="describe(r).detail" class="detail">{{ describe(r).detail }}</pre>
          </div>
          <span v-if="r.kind === 'event'" class="seq faint tabular">#{{ r.e.seq }}</span>
          <span v-else class="seq faint tabular" :title="`${r.j.method} ${r.j.path}`">{{ r.j.via }}</span>
        </li>
      </ol>
      <div v-else class="empty">
        <Icon name="activity" :size="28" />
        <p v-if="!rows.length">{{ stack.live ? 'Waiting for events…' : 'Not connected to the stack.' }}</p>
        <p v-else>Nothing matches the filter.</p>
      </div>
    </section>
  </div>
</template>

<style scoped>
.filters {
  flex-wrap: wrap;
  gap: 8px;
}
.input-sm {
  height: 28px;
  font-size: 12.5px;
}
.timeline {
  list-style: none;
  margin: 0;
  padding: 6px 0;
}
.ev {
  --tone: var(--text-faint);
  --tone-text: var(--text-muted);
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 7px 16px;
  font-size: 13px;
  animation: in 0.25s var(--ease);
}
.ev:hover {
  background: var(--surface-hover);
}
@keyframes in {
  from {
    opacity: 0;
    transform: translateY(-3px);
  }
}
.tone-ok {
  --tone: var(--ok);
  --tone-text: var(--ok-text);
}
.tone-warn {
  --tone: var(--warn);
  --tone-text: var(--warn-text);
}
.tone-danger {
  --tone: var(--danger);
  --tone-text: var(--danger-text);
}
.tone-accent {
  --tone: var(--accent);
  --tone-text: var(--accent-text);
}
.tone-info {
  --tone: var(--info);
  --tone-text: var(--info-text);
}
.t {
  color: var(--text-muted);
  font-size: 12px;
  width: 64px;
  flex: none;
  padding-top: 2px;
  font-family: var(--font-mono);
}
.ic {
  width: 24px;
  height: 24px;
  border-radius: 7px;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--tone) 15%, transparent);
  color: var(--tone-text);
  flex: none;
}
.body {
  min-width: 0;
}
.line1 {
  display: flex;
  gap: 8px;
  align-items: baseline;
  flex-wrap: wrap;
}
.ty {
  font-size: 10.5px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  font-weight: 700;
  color: var(--text-faint);
  min-width: 64px;
}
.subj {
  font-weight: 600;
  color: var(--text);
}
.txt {
  color: var(--tone-text);
}
.detail {
  margin: 4px 0 0;
  font-family: var(--font-mono);
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-2);
  background: var(--surface-2);
  border-radius: 6px;
  padding: 6px 8px;
}
.seq {
  font-size: 11px;
  padding-top: 3px;
}
@media (max-width: 520px) {
  .t {
    width: auto;
  }
  .ty {
    min-width: 0;
  }
  .seq {
    display: none;
  }
}
</style>
