<script setup lang="ts">
import { computed, ref } from 'vue';
import { clock, shortBuild } from '@/lib/format';
import { candidateLook, candidateSummary, rebuildLook, rebuildSummary, statusLook, type Tone } from '@/lib/status';
import { useStack, type TimelineEvent } from '@/stores/stack';
import Icon from '@/components/icons/Icon.vue';
import type { IconName } from '@/components/icons/icons';

type Type = TimelineEvent['type'];
const stack = useStack();
const types: { v: Type; label: string }[] = [
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
const enabled = ref<Record<Type, boolean>>({ service: true, candidate: true, devserver_rebuild: true, infra: true, command: true, config: true, watcher: true, notice: true, snapshot: true });
const subject = ref('');

function subjectOf(e: TimelineEvent): string {
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

const subjects = computed(() => {
  const set = new Set<string>();
  for (const s of stack.view?.services ?? []) set.add(s.name);
  for (const e of stack.events) {
    const s = subjectOf(e);
    if (s) set.add(s);
  }
  return [...set].sort();
});

const shown = computed(() => stack.events.filter((e) => enabled.value[e.type] && (!subject.value || subjectOf(e) === subject.value)));

type Desc = { icon: IconName; tone: Tone; title: string; text: string; detail?: string | null };

function describe(e: TimelineEvent): Desc {
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
        <h1>Events</h1>
        <p class="subtitle">The stack's live event stream since this page connected, newest first (last 500).</p>
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
        <label class="sr-only" for="ev-subject">Filter by service</label>
        <select id="ev-subject" v-model="subject" class="input input-sm" data-testid="events-subject">
          <option value="">All services</option>
          <option v-for="s in subjects" :key="s" :value="s">{{ s }}</option>
        </select>
      </header>
      <ol v-if="shown.length" class="timeline stale-dim" data-testid="events-list" aria-live="polite">
        <li v-for="e in shown" :key="e.seq" class="ev" :class="`tone-${describe(e).tone}`" :data-type="e.type">
          <time class="t tabular" :datetime="new Date(e.at).toISOString()">{{ clock(e.at) }}</time>
          <span class="ic"><Icon :name="describe(e).icon" :size="14" /></span>
          <div class="grow body">
            <div class="line1">
              <span class="ty">{{ e.type }}</span>
              <RouterLink v-if="(e.type === 'service' || e.type === 'candidate') && stack.svc(e.service)" :to="{ name: 'service', params: { stack: stack.id ?? '', name: e.service } }" class="subj">{{ describe(e).title }}</RouterLink>
              <span v-else class="subj">{{ describe(e).title }}</span>
              <span class="txt">{{ describe(e).text }}</span>
            </div>
            <pre v-if="describe(e).detail" class="detail">{{ describe(e).detail }}</pre>
          </div>
          <span class="seq faint tabular">#{{ e.seq }}</span>
        </li>
      </ol>
      <div v-else class="empty">
        <Icon name="activity" :size="28" />
        <p v-if="!stack.events.length">{{ stack.live ? 'Waiting for events…' : 'Not connected to the stack.' }}</p>
        <p v-else>No events match the filter.</p>
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
