<script setup lang="ts">
// The builds of a service (plan §6): which went live, failed, were replaced
// by a newer change or wait for promote. From the daemon's diagnostic log,
// so it reaches back across daemon restarts. A row opens its build log.
import { computed, ref, watch } from 'vue';
import { api, paths, v1 } from '@/api';
import { ago, dateTime, duration, shortBuild, useNow } from '@/lib/format';
import { useSort } from '@/lib/sort';
import { useStack } from '@/stores/stack';
import KeptLogsModal from './KeptLogsModal.vue';
import SortTh from './SortTh.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ service: string; build: string | null; candidateState: string | null }>();
const stack = useStack();
const now = useNow();

/** `artifact`, `log`: what of it is still there (older daemons omit both: taken as there). */
type Build = { build: string; started_ms: number; updated_ms: number; outcome: string; reason: string | null; current: boolean; artifact?: boolean; log?: boolean };
const hasLog = (b: Build) => b.log !== false;
function rowTitle(b: Build): string {
  if (hasLog(b)) return `Build log of ${b.build}${b.artifact === false ? '\nIts artifact is removed (the newest five per service are kept)' : ''}`;
  return `${b.build}: its artifact and build log are removed (the newest five artifacts and ten build logs per service are kept)`;
}
function open(b: Build) {
  if (hasLog(b)) logFor.value = b.build;
}
const list = ref<Build[]>([]);
const loaded = ref(false);
const logFor = ref<string | null>(null);

async function load() {
  if (!stack.id || !stack.running) return;
  try {
    const res = await api().request<Build[]>('GET', paths.api(stack.id, v1.builds(props.service), stack.apiLane));
    if (res.ok && Array.isArray(res.body)) list.value = res.body;
  } catch {
    // keep what is shown; the next change loads again
  } finally {
    loaded.value = true;
  }
}

// A new build or a candidate step changes the list.
watch(() => [props.service, props.build, props.candidateState, stack.running] as const, load, { immediate: true });

const sort = useSort(
  list,
  {
    build: (b) => b.build,
    outcome: (b) => b.outcome,
    started: (b) => b.started_ms,
    duration: (b) => b.updated_ms - b.started_ms,
    reason: (b) => b.reason,
  },
  { id: 'builds' },
);
const rows = sort.sorted;

const tone = (o: string) => (o === 'live' ? 'ok' : o === 'failed' ? 'danger' : o === 'held' ? 'warn' : o === 'superseded' || o === 'interrupted' ? '' : 'accent');
/** The newest few; the rest on request. */
const FIRST = 10;
const expanded = ref(false);
const shown = computed(() => (expanded.value ? rows.value : rows.value.slice(0, FIRST)));
</script>

<template>
  <section class="card" data-testid="build-timeline">
    <header class="card-header">
      <h2>Builds</h2>
      <span class="count">{{ list.length }}</span>
      <span class="grow" />
      <button type="button" class="btn btn-ghost btn-sm btn-icon" title="Reload" aria-label="Reload builds" @click="load"><Icon name="restart" /></button>
    </header>
    <div v-if="shown.length" class="table-wrap">
      <table class="table">
        <thead>
          <tr>
            <SortTh label="Build" :dir="sort.dirOf('build')" @toggle="sort.toggle('build')" />
            <SortTh label="Outcome" :dir="sort.dirOf('outcome')" @toggle="sort.toggle('outcome')" />
            <SortTh label="Started" :dir="sort.dirOf('started')" @toggle="sort.toggle('started')" />
            <SortTh label="Took" class="num" :dir="sort.dirOf('duration')" @toggle="sort.toggle('duration')" />
            <SortTh label="Reason" class="hide-sm" :dir="sort.dirOf('reason')" @toggle="sort.toggle('reason')" />
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="b in shown"
            :key="b.build"
            class="build-row"
            :class="{ gone: !hasLog(b) }"
            :tabindex="hasLog(b) ? 0 : undefined"
            :title="rowTitle(b)"
            data-testid="build-row"
            @click="open(b)"
            @keydown.enter="open(b)"
          >
            <td class="mono nowrap">
              {{ shortBuild(b.build) }}<Icon v-if="b.artifact" name="box" :size="12" class="kept" aria-label="artifact kept" /><span v-if="b.current" class="tag now">running</span>
            </td>
            <td><span class="pill sm" :class="tone(b.outcome)">{{ b.outcome }}</span></td>
            <td class="nowrap" :title="dateTime(b.started_ms)">{{ ago(b.started_ms, now) }}</td>
            <td class="num">{{ duration(b.updated_ms - b.started_ms) }}</td>
            <td class="hide-sm err truncate">{{ b.reason?.split('\n')[0] ?? '' }}</td>
          </tr>
        </tbody>
      </table>
      <button v-if="list.length > FIRST" type="button" class="more" :aria-expanded="expanded" data-testid="builds-more" @click="expanded = !expanded">
        <Icon :name="expanded ? 'chevronDown' : 'chevronRight'" :size="13" />
        {{ expanded ? 'Show fewer' : `Show ${list.length - FIRST} more` }}
      </button>
    </div>
    <div v-else class="empty"><p>{{ loaded ? 'No build in the diagnostic log yet.' : 'Loading…' }}</p></div>
    <KeptLogsModal v-if="logFor && stack.id" :open="!!logFor" :stack-id="stack.id" :lane="stack.apiLane" :service="service" :initial-file="`build-${logFor}.log`" @close="logFor = null" />
  </section>
</template>

<style scoped>
.build-row {
  cursor: pointer;
}
/* Its log is gone: what happened stays, nothing opens. */
.build-row.gone {
  cursor: default;
  color: var(--text-muted);
}
.build-row.gone .pill {
  opacity: 0.6;
}
.kept {
  margin-left: 5px;
  color: var(--text-faint);
  vertical-align: -1px;
}
.more {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  width: 100%;
  padding: 8px;
  border: 0;
  border-top: 1px solid var(--border);
  background: transparent;
  color: var(--text-muted);
  font: inherit;
  font-size: 12.5px;
  cursor: pointer;
}
.more:hover {
  background: var(--surface-hover);
  color: var(--text);
}
.now {
  margin-left: 6px;
  font-size: 11px;
}
.err {
  color: var(--danger);
  max-width: 360px;
}
</style>
