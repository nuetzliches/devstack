<script setup lang="ts">
// The coding agents' sessions in this lane's working tree, one at a time in
// time: what devstack recorded for them (its hooks, the calls through
// `devstack mcp`) and, once switched on, what the agent's transcript tells:
// its turns with their tokens, its own tools, when a prompt came. Never a
// prompt's text, a tool's arguments or its answer. Read again on each
// `activity` event, and every few seconds while the session runs.
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { api, paths } from '@/api';
import type { ActivityRecord, ActivityView, SessionTimeline as Timeline, TranscriptSetting } from '@/api/types';
import { act } from '@/lib/actions';
import { recordText, sessionLabel } from '@/lib/flow';
import { useNow } from '@/lib/format';
import { useHub } from '@/stores/hub';
import SessionTimeline, { type LaneMark } from './SessionTimeline.vue';

const props = defineProps<{ tree: string; lane: LaneMark[] }>();
const hub = useHub();
const now = useNow();

/** A working tree as the hub writes it: forward slashes, no slash at the end; case is ignored. */
const norm = (p: string) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();

const activity = ref<ActivityView | null>(null);
const setting = ref<TranscriptSetting | null>(null);
const timeline = ref<Timeline | null>(null);
const problem = ref<string | null>(null);
const picked = ref('');

const sessions = computed(() => (activity.value?.sessions ?? []).filter((s) => norm(s.tree) === norm(props.tree)));
const session = computed(() => sessions.value.find((s) => s.session === picked.value) ?? sessions.value[0] ?? null);
const records = computed<ActivityRecord[]>(() => (activity.value?.records ?? []).filter((r) => session.value && r.session === session.value.session).slice().reverse());

async function get<T>(path: string): Promise<T | null> {
  try {
    const r = await api().request<T>('GET', path);
    if (r.ok && r.body && typeof r.body === 'object') return r.body;
    problem.value = r.status === 404 ? 'This hub does not show the agents\' sessions in time yet: update devstack.' : `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
  return null;
}

async function readSessions() {
  const [a, s] = await Promise.all([get<ActivityView>(paths.activity({ tree: props.tree })), setting.value ? Promise.resolve(setting.value) : get<TranscriptSetting>(paths.transcripts)]);
  if (a) activity.value = a;
  if (s) setting.value = s;
}
async function readTimeline() {
  const s = session.value;
  if (!s) return void (timeline.value = null);
  const t = await get<Timeline>(paths.timeline(s.session, s.agent));
  if (t && session.value?.session === t.session) timeline.value = t;
}
watch([() => props.tree, () => hub.activityAt], () => void readSessions(), { immediate: true });
watch(() => [session.value?.session, session.value?.last, setting.value?.enabled], () => void readTimeline(), { immediate: true });

// A running session's transcript grows without an `activity` event: read it again now and then.
const timer = setInterval(() => {
  if (session.value && !session.value.ended && document.visibilityState === 'visible') void readTimeline();
}, 5000);
onBeforeUnmount(() => clearInterval(timer));

async function setTranscripts(enabled: boolean) {
  const r = await act<TranscriptSetting>({ label: enabled ? 'Read the agents\' transcripts' : 'Stop reading the agents\' transcripts', method: 'PUT', path: paths.transcripts, body: { enabled }, success: false });
  if (r.ok && r.body && typeof r.body === 'object') setting.value = r.body;
}

const skillOf = (key: string) => key.split('/')[2]?.split('#')[0];
</script>

<template>
  <section class="card" data-testid="agent-timeline">
    <header class="card-header head">
      <h2>Agent sessions</h2>
      <select v-if="sessions.length" v-model="picked" class="input input-sm" data-testid="agent-timeline-session">
        <option value="">Latest: {{ sessions[0] ? sessionLabel(sessions[0]) : '' }}</option>
        <option v-for="s in sessions.slice(1)" :key="`${s.agent}/${s.session}`" :value="s.session">{{ sessionLabel(s) }}{{ s.ended ? ' (ended)' : '' }}</option>
      </select>
      <span class="grow" />
      <label v-if="setting" class="switch small" title="Read the agents' transcripts for their turns, tokens and own tools: only times, counts and names">
        <input type="checkbox" :checked="setting.enabled" data-testid="agent-timeline-transcripts" @change="setTranscripts(($event.target as HTMLInputElement).checked)" />
        Read transcripts
      </label>
    </header>
    <div class="card-body">
      <p v-if="problem" class="err small">{{ problem }}</p>
      <p v-else-if="activity && !sessions.length" class="muted small" data-testid="agent-timeline-empty">
        No agent session in this working tree yet. A session shows once a coding agent with devstack's hooks starts here.
      </p>
      <template v-if="timeline && session">
        <p v-if="timeline.why === 'off'" class="note small" data-testid="agent-timeline-off">
          Only what devstack recorded: its hooks and the calls through it. To see the agent's turns, its tokens, its own tools and when a prompt came, switch on
          <button type="button" class="btn-link" @click="setTranscripts(true)">Read transcripts</button>. The hub then reads the session's transcript on this machine when you look, takes only times, token counts and
          tool names from it, and keeps nothing.
        </p>
        <p v-else-if="timeline.why === 'agent'" class="note small">devstack cannot read this agent's transcript yet: only what devstack recorded shows.</p>
        <p v-else-if="timeline.why === 'missing'" class="note small">The session's transcript was not found (moved, or deleted): only what devstack recorded shows.</p>
        <SessionTimeline :timeline="timeline" :lane="lane" :now="now" />
        <details class="records-box">
          <summary class="small muted">What devstack recorded in this session ({{ records.length }})</summary>
          <ol class="records">
            <li v-for="(r, i) in records" :key="i" class="record" :class="{ failed: r.kind === 'tool' && !r.ok }" data-testid="agent-timeline-record">
              <span class="t faint small">{{ new Date(r.at).toLocaleTimeString() }}</span>
              <span>{{ recordText(r, skillOf).title }}</span>
              <span class="faint small">{{ recordText(r, skillOf).detail }}</span>
            </li>
          </ol>
        </details>
      </template>
      <p class="faint small foot">Only what happened: never a prompt, a hook's text, a tool's arguments or its answer. Kept on this machine for a week.</p>
    </div>
  </section>
</template>

<style scoped>
.head {
  gap: 10px;
  flex-wrap: wrap;
}
.head select {
  max-width: 420px;
}
.grow {
  flex: 1;
}
.input-sm {
  height: 28px;
  font-size: 12.5px;
}
.switch {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  color: var(--text-muted);
  cursor: pointer;
}
.note {
  margin: 0 0 10px;
  padding: 8px 10px;
  border-radius: 6px;
  background: var(--info-soft);
  color: var(--text);
}
.btn-link {
  border: 0;
  padding: 0;
  background: none;
  color: var(--accent-text);
  text-decoration: underline;
  cursor: pointer;
  font: inherit;
}
.err {
  color: var(--danger-text);
}
.records-box {
  margin-top: 10px;
}
.records {
  list-style: none;
  margin: 6px 0 0;
  padding: 0;
  max-height: 320px;
  overflow: auto;
}
.record {
  display: grid;
  grid-template-columns: 80px minmax(0, 260px) 1fr;
  gap: 8px;
  padding: 3px 4px;
  font-size: 12.5px;
}
.record.failed {
  color: var(--danger-text);
}
.foot {
  margin: 10px 0 0;
}
</style>
