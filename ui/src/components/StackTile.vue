<script setup lang="ts">
// One stack on the start page: its state, a few figures the hub knows for
// every stack (the `summary` event: services ready of all in the main tree,
// lanes started; the list: up since, last commit, port shift), what needs
// attention, and the favourite programs to open its main tree with. The
// name is the link to the stack's page and covers the tile; the buttons sit
// above it.
import { computed } from 'vue';
import type { StackEntry } from '@/api/types';
import { ago, duration, useNow } from '@/lib/format';
import { stackPage } from '@/lib/stackRoutes';
import type { Tone } from '@/lib/status';
import { useHub } from '@/stores/hub';
import Icon from './icons/Icon.vue';
import StatusPill from './StatusPill.vue';
import FavoriteOpeners from './FavoriteOpeners.vue';

const props = defineProps<{ stack: StackEntry; favourite: boolean }>();
const emit = defineEmits<{ toggleFavourite: [] }>();

const hub = useHub();
const now = useNow();

/** Most attention lines on a tile; the rest are counted. */
const SHOWN_ITEMS = 2;

const glance = computed(() => hub.glances[props.stack.id] ?? null);
const items = computed(() => hub.attention.filter((a) => a.stack === props.stack.id));

const look = computed((): { label: string; tone: Tone; busy: boolean } => {
  const s = props.stack;
  if (!hub.reachable) return { label: 'Unknown', tone: 'neutral', busy: false };
  // A start or stop this page asked for, before the hub's figures say it.
  if (hub.stopping[s.id]) return { label: 'Stopping', tone: 'neutral', busy: true };
  if (hub.starting[s.id] && !s.running) return { label: 'Starting', tone: 'accent', busy: true };
  const state = glance.value?.state ?? (s.start_error ? 'attention' : s.running ? 'ok' : 'stopped');
  switch (state) {
    case 'attention':
      return { label: 'Needs attention', tone: items.value.some((i) => i.level === 'error') || s.start_error ? 'danger' : 'warn', busy: false };
    case 'starting':
      return { label: 'Starting', tone: 'accent', busy: true };
    case 'stopping':
      return { label: 'Stopping', tone: 'neutral', busy: true };
    case 'ok':
      return { label: 'Running', tone: 'ok', busy: false };
    default:
      return { label: 'Stopped', tone: 'neutral', busy: false };
  }
});

/** The path's folder and its last part: a long path is cut in the folder, the repository's name stays. */
const parts = computed(() => {
  const m = /^(.*[\\/])([^\\/]+)[\\/]?$/.exec(props.stack.repo);
  return m ? { dir: m[1], leaf: m[2] } : { dir: '', leaf: props.stack.repo };
});

const services = computed(() => {
  const g = glance.value;
  if (!props.stack.running || !g || !g.total) return null;
  return { ready: g.ready, total: g.total, share: g.ready / g.total };
});
const lanes = computed(() => (props.stack.running && glance.value ? glance.value.lanes : null));
const upFor = computed(() => (props.stack.running && props.stack.daemon ? duration(now.value - props.stack.daemon.started) : null));
const DAY = 86_400_000;
/** Within a day as `ago` says it; older in days, which fits a tile better than a date. */
const committed = computed(() => {
  const at = props.stack.last_commit ? props.stack.last_commit * 1000 : null;
  if (!at) return null;
  const days = Math.floor((now.value - at) / DAY);
  return days >= 1 ? `${days} d ago` : ago(at, now.value);
});

const shownItems = computed(() => items.value.slice(0, SHOWN_ITEMS));
const moreItems = computed(() => items.value.length - shownItems.value.length);
</script>

<template>
  <article class="tile" :class="{ running: stack.running }" data-testid="stack-tile" :data-id="stack.id">
    <header class="head">
      <button
        type="button"
        class="star"
        :class="{ on: favourite }"
        :aria-pressed="favourite"
        :title="favourite ? 'Remove from favourites' : 'Add to favourites'"
        :aria-label="favourite ? `Remove ${stack.name} from favourites` : `Add ${stack.name} to favourites`"
        data-testid="tile-star"
        @click="emit('toggleFavourite')"
      >
        <Icon name="star" :size="14" />
      </button>
      <RouterLink :to="stackPage(stack.id)" class="name" data-testid="tile-link">{{ stack.name }}</RouterLink>
      <StatusPill :tone="look.tone" :label="look.label" :busy="look.busy" size="sm" data-testid="tile-state" />
    </header>

    <div class="path mono faint" :title="stack.repo">
      <span class="dir">{{ parts.dir }}</span><span class="leaf">{{ parts.leaf }}</span>
    </div>

    <dl class="figures">
      <div v-if="services" class="fig" data-testid="tile-services">
        <dt>Services</dt>
        <dd>
          <span class="num">{{ services.ready }}<span class="faint">/{{ services.total }}</span></span>
          <span class="bar" aria-hidden="true"><span :style="{ width: `${services.share * 100}%` }" :class="{ full: services.ready === services.total }" /></span>
        </dd>
      </div>
      <div v-if="lanes !== null" class="fig" data-testid="tile-lanes">
        <dt>Lanes</dt>
        <dd class="num">{{ lanes }}</dd>
      </div>
      <div v-if="upFor" class="fig" data-testid="tile-up">
        <dt>Up</dt>
        <dd>{{ upFor }}</dd>
      </div>
      <div v-if="committed" class="fig" data-testid="tile-commit">
        <dt>Last commit</dt>
        <dd>{{ committed }}</dd>
      </div>
      <div v-if="stack.shift" class="fig">
        <dt>Ports</dt>
        <dd class="mono">+{{ stack.shift }}</dd>
      </div>
    </dl>

    <ul v-if="items.length || stack.no_file" class="notes">
      <li v-if="stack.no_file" class="note warn"><Icon name="alert" :size="13" /> Its stack file is missing</li>
      <li v-for="(i, n) in shownItems" :key="n" class="note" :class="i.level" :title="i.detail || undefined" data-testid="tile-attention">
        <Icon :name="i.level === 'warn' ? 'pause' : 'xCircle'" :size="13" />
        <span class="text"><span v-if="i.lane" class="faint">{{ i.lane }}: </span>{{ i.text }}</span>
      </li>
      <li v-if="moreItems > 0" class="note faint">+{{ moreItems }} more</li>
    </ul>

    <footer class="foot">
      <FavoriteOpeners :stack-id="stack.id" :path="stack.repo" />
    </footer>
  </article>
</template>

<style scoped>
.tile {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-width: 0;
  padding: 14px 16px;
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--surface);
  transition:
    border-color 0.12s,
    box-shadow 0.12s;
}
.tile:hover,
.tile:focus-within {
  border-color: var(--border-strong);
  box-shadow: var(--shadow-sm);
}
.head {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: 15px;
  font-weight: 600;
  text-decoration: none;
}
/* The name's link covers the whole tile; buttons sit above it. */
.name::after {
  content: '';
  position: absolute;
  inset: 0;
  border-radius: inherit;
}
.name:focus-visible {
  outline: none;
}
.name:focus-visible::after {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}
.star,
.foot,
.notes {
  position: relative;
  z-index: 1;
}
.star {
  display: grid;
  place-items: center;
  width: 22px;
  height: 22px;
  flex: none;
  padding: 0;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--text-faint);
  cursor: pointer;
}
.star:hover {
  background: var(--surface-hover);
  color: var(--text);
}
.star.on {
  color: var(--warn);
}
.star.on :deep(svg) {
  fill: currentColor;
}
.path {
  display: flex;
  min-width: 0;
  font-size: 12px;
}
.path .dir {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.path .leaf {
  flex: none;
  max-width: 70%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.figures {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 18px;
  margin: 0;
}
.fig {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.fig dt {
  color: var(--text-2);
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.fig dd {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0;
  font-size: 13px;
}
.num {
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}
.bar {
  width: 48px;
  height: 4px;
  border-radius: 999px;
  background: var(--surface-2);
  overflow: hidden;
}
.bar span {
  display: block;
  height: 100%;
  border-radius: inherit;
  background: var(--warn);
}
.bar span.full {
  background: var(--ok);
}
.notes {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
  font-size: 12.5px;
}
.note {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.note .text {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.note.error {
  color: var(--danger-text);
}
.note.warn {
  color: var(--warn-text);
}
.foot {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: auto;
}
.foot:empty {
  display: none;
}
</style>
