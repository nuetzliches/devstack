<script setup lang="ts">
// A live log on an SSE stream: auto-scroll with "jump to latest", pause,
// search with highlighting (or only matching lines), wrap toggle, level
// colouring, a marker when another instance takes over. Keeps `maxLines`.
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from 'vue';
import { api, type StreamState, type Subscription } from '@/api';
import type { LogStreamEvent, RunLogEvent } from '@/api/types';
import { shortBuild } from '@/lib/format';
import Icon from './icons/Icon.vue';

type Level = 'error' | 'warn' | 'debug' | 'info';
type LogLine = { id: number; text: string; level: Level; marker?: 'instance' | 'reconnect' | 'end-ok' | 'end-fail' };

const props = withDefaults(
  defineProps<{
    path: string | null;
    maxLines?: number;
    height?: string;
    emptyText?: string;
    /** Run logs end with an `end` event and do not reconnect after it. */
    kind?: 'service' | 'run';
    toolbar?: boolean;
    testid?: string;
  }>(),
  { maxLines: 5000, height: '480px', emptyText: 'No output yet.', kind: 'service', toolbar: true, testid: 'log-view' },
);
const emit = defineEmits<{ end: [status: 'succeeded' | 'failed', error: string | null] }>();

const lines = shallowRef<LogLine[]>([]);
const state = ref<StreamState>('connecting');
const paused = ref(false);
const pausedCount = ref(0);
const follow = ref(true);
const wrap = ref(true);
const query = ref('');
const onlyMatches = ref(false);
const ended = ref(false);
const scroller = ref<HTMLElement | null>(null);

let nextId = 1;
let sub: Subscription | null = null;
let buffer: LogLine[] = [];
let frame: ReturnType<typeof setTimeout> | null = null;
let opens = 0;

function level(text: string): Level {
  if (/\b(fail|error|err|fatal|crit|panic|exception)\b|\bERROR\b/i.test(text)) return 'error';
  if (/\b(warn|warning)\b/i.test(text)) return 'warn';
  if (/\b(dbug|debug|trce|trace|verbose)\b/i.test(text)) return 'debug';
  return 'info';
}

function push(l: Omit<LogLine, 'id'>) {
  buffer.push({ ...l, id: nextId++ });
  if (paused.value) {
    pausedCount.value++;
    return;
  }
  // batch bursts; a timer rather than rAF so background tabs keep up
  if (!frame) frame = setTimeout(flush, 40);
}

function flush() {
  frame = null;
  if (!buffer.length) return;
  // a new array, so computed views over it see the change
  const arr = lines.value.concat(buffer);
  buffer = [];
  if (arr.length > props.maxLines) arr.splice(0, arr.length - props.maxLines);
  lines.value = arr;
  if (follow.value) void nextTick(scrollToEnd);
}

function scrollToEnd() {
  const el = scroller.value;
  if (el) el.scrollTop = el.scrollHeight;
}

function onScroll() {
  const el = scroller.value;
  if (!el) return;
  follow.value = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
}

function jumpToLatest() {
  follow.value = true;
  if (paused.value) togglePause();
  void nextTick(scrollToEnd);
}

function togglePause() {
  paused.value = !paused.value;
  if (!paused.value) {
    pausedCount.value = 0;
    flush();
  }
}

function clear() {
  lines.value = [];
  buffer = [];
  pausedCount.value = 0;
}

function connect(path: string | null) {
  sub?.close();
  sub = null;
  clear();
  ended.value = false;
  opens = 0;
  if (!path) return;
  sub = api().subscribe<LogStreamEvent | RunLogEvent>(path, {
    onMessage(ev) {
      if (ev.type === 'line') push({ text: ev.text, level: level(ev.text) });
      else if (ev.type === 'instance') push({ text: `New instance took over · build ${shortBuild(ev.build_id)}`, level: 'info', marker: 'instance' });
      else if (ev.type === 'end') {
        ended.value = true;
        push({
          text: ev.status === 'succeeded' ? 'Finished successfully' : `Failed${ev.error ? `: ${ev.error}` : ''}`,
          level: ev.status === 'succeeded' ? 'info' : 'error',
          marker: ev.status === 'succeeded' ? 'end-ok' : 'end-fail',
        });
        emit('end', ev.status, ev.error);
        sub?.close();
        sub = null;
      }
    },
    onState(s) {
      state.value = s;
      if (s === 'open') {
        // a reconnect replays the tail: start over instead of duplicating it
        if (opens++ > 0) {
          clear();
          push({ text: 'Reconnected', level: 'info', marker: 'reconnect' });
        }
      }
    },
  });
}

watch(() => props.path, connect, { immediate: true });

onBeforeUnmount(() => {
  sub?.close();
  if (frame) clearTimeout(frame);
});

const needle = computed(() => query.value.trim().toLowerCase());
const matchCount = computed(() => (needle.value ? lines.value.filter((l) => l.text.toLowerCase().includes(needle.value)).length : 0));
const visible = computed(() => (needle.value && onlyMatches.value ? lines.value.filter((l) => l.marker || l.text.toLowerCase().includes(needle.value)) : lines.value));

function parts(text: string): { t: string; m: boolean }[] {
  const n = needle.value;
  if (!n) return [{ t: text, m: false }];
  const out: { t: string; m: boolean }[] = [];
  const low = text.toLowerCase();
  let i = 0;
  for (;;) {
    const j = low.indexOf(n, i);
    if (j < 0) break;
    if (j > i) out.push({ t: text.slice(i, j), m: false });
    out.push({ t: text.slice(j, j + n.length), m: true });
    i = j + n.length;
  }
  if (i < text.length) out.push({ t: text.slice(i), m: false });
  return out;
}

const stateLabel = computed(() => {
  if (ended.value) return 'Ended';
  if (state.value === 'open') return paused.value ? 'Paused' : 'Live';
  if (state.value === 'error') return 'Reconnecting…';
  return 'Connecting…';
});

watch(onlyMatches, () => {
  if (follow.value) void nextTick(scrollToEnd);
});

defineExpose({ clear });
</script>

<template>
  <div class="logview" :data-testid="testid">
    <div v-if="toolbar" class="log-toolbar">
      <span class="live" :class="{ on: state === 'open' && !paused && !ended, warn: state === 'error', ended }" data-testid="log-state">
        <span class="dot" aria-hidden="true" />{{ stateLabel }}
      </span>
      <div class="input-wrap search">
        <Icon name="search" :size="14" />
        <input v-model="query" class="input input-sm" type="search" placeholder="Filter…" aria-label="Filter log lines" data-testid="log-filter" />
      </div>
      <span v-if="needle" class="muted small tabular" aria-live="polite">{{ matchCount }} match{{ matchCount === 1 ? '' : 'es' }}</span>
      <button v-if="needle" type="button" class="btn btn-sm" :aria-pressed="onlyMatches" title="Show only matching lines" @click="onlyMatches = !onlyMatches">
        <Icon name="filter" :size="14" /><span class="hide-xs">Only matches</span>
      </button>
      <span class="grow" />
      <button v-if="kind === 'service'" type="button" class="btn btn-sm" :aria-pressed="paused" data-testid="log-pause" @click="togglePause">
        <Icon :name="paused ? 'play' : 'pause'" :size="14" /><span class="hide-xs">{{ paused ? 'Resume' : 'Pause' }}</span>
      </button>
      <button type="button" class="btn btn-sm" :aria-pressed="wrap" title="Wrap long lines" data-testid="log-wrap" @click="wrap = !wrap">
        <Icon name="wrap" :size="14" /><span class="hide-xs">Wrap</span>
      </button>
      <slot name="actions" />
    </div>
    <div class="log-body" :style="{ height }">
      <div ref="scroller" class="log-scroll" :class="{ nowrap: !wrap }" tabindex="0" role="log" aria-label="Log output" @scroll.passive="onScroll">
        <div v-if="!visible.length" class="log-empty">
          <template v-if="state === 'open' || ended">{{ needle && onlyMatches ? 'No matching lines.' : emptyText }}</template>
          <template v-else-if="state === 'error'">Waiting for the stream to come back…</template>
          <template v-else>Connecting…</template>
        </div>
        <template v-for="l in visible" :key="l.id">
          <div v-if="l.marker" class="marker" :class="l.marker" data-testid="log-marker">
            <span>{{ l.text }}</span>
          </div>
          <div v-else class="line" :class="l.level"><template v-for="(p, i) in parts(l.text)" :key="i"><mark v-if="p.m">{{ p.t }}</mark><template v-else>{{ p.t }}</template></template></div>
        </template>
      </div>
      <Transition name="fade">
        <button v-if="!follow || paused" type="button" class="btn btn-sm btn-primary jump" data-testid="log-jump" @click="jumpToLatest">
          <Icon name="arrowDown" :size="14" />
          {{ paused && pausedCount ? `${pausedCount} new line${pausedCount === 1 ? '' : 's'}` : 'Jump to latest' }}
        </button>
      </Transition>
    </div>
  </div>
</template>

<style scoped>
.logview {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.log-toolbar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 10px;
  border-bottom: 1px solid var(--border);
  flex-wrap: wrap;
}
.search {
  width: min(240px, 100%);
  flex: 0 1 240px;
}
.input-sm {
  height: 28px;
  font-size: 12.5px;
}
.small {
  font-size: 12px;
}
.live {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  color: var(--text-muted);
  min-width: 92px;
}
.live .dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--text-faint);
}
.live.on {
  color: var(--ok-text);
}
.live.on .dot {
  background: var(--ok);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 25%, transparent);
  animation: pulse 2s ease-in-out infinite;
}
.live.warn {
  color: var(--warn-text);
}
.live.warn .dot {
  background: var(--warn);
}
.log-body {
  position: relative;
  min-height: 160px;
}
.log-scroll {
  position: absolute;
  inset: 0;
  overflow: auto;
  background: var(--log-bg);
  font-family: var(--font-mono);
  font-size: 12.5px;
  line-height: 1.55;
  padding: 8px 0;
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
}
.log-scroll:focus-visible {
  box-shadow: inset var(--focus);
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
}
.line {
  padding: 0 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text-2);
}
.nowrap .line {
  white-space: pre;
  overflow-wrap: normal;
  width: max-content;
  min-width: 100%;
}
.line:hover {
  background: var(--surface-hover);
}
.line.error {
  color: var(--log-error);
  background: color-mix(in srgb, var(--danger) 6%, transparent);
}
.line.warn {
  color: var(--log-warn);
}
.line.debug {
  color: var(--log-debug);
}
mark {
  background: var(--mark);
  color: var(--mark-text);
  border-radius: 2px;
  padding: 0 1px;
}
.marker {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 6px 12px;
  font-family: var(--font-sans);
  font-size: 11.5px;
  font-weight: 600;
  color: var(--accent-text);
}
.marker::before,
.marker::after {
  content: '';
  flex: 1;
  height: 1px;
  background: color-mix(in srgb, var(--accent) 40%, transparent);
}
.marker.reconnect {
  color: var(--text-muted);
}
.marker.reconnect::before,
.marker.reconnect::after {
  background: var(--border-strong);
}
.marker.end-ok {
  color: var(--ok-text);
}
.marker.end-ok::before,
.marker.end-ok::after {
  background: color-mix(in srgb, var(--ok) 45%, transparent);
}
.marker.end-fail {
  color: var(--danger-text);
}
.marker.end-fail::before,
.marker.end-fail::after {
  background: color-mix(in srgb, var(--danger) 45%, transparent);
}
.log-empty {
  padding: 24px 12px;
  color: var(--text-faint);
  font-family: var(--font-sans);
  text-align: center;
}
.jump {
  position: absolute;
  bottom: 14px;
  left: 50%;
  transform: translateX(-50%);
  box-shadow: var(--shadow-lg);
  border-radius: 999px;
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.15s;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
@media (max-width: 520px) {
  .hide-xs {
    display: none;
  }
  .search {
    flex: 1 1 140px;
  }
}
</style>
