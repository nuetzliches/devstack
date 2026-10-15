<script setup lang="ts">
// The prominent "not yet active" panel of a service: what is on its way, how
// far it got, and why it did not arrive (compiler errors, schema hold), and
// the console of the candidate's own process while it starts.
import { computed, ref } from 'vue';
import { paths, v1 } from '@/api';
import type { ServiceView } from '@/api/types';
import { ago, shortBuild, useNow } from '@/lib/format';
import { candidateLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import Diagnostics from './Diagnostics.vue';
import LogView from './LogView.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ service: ServiceView; readonly?: boolean }>();
const emit = defineEmits<{ buildLog: [] }>();
const stack = useStack();
const now = useNow();

const c = computed(() => props.service.candidate!);
const look = computed(() => candidateLook(c.value));
const busy = computed(() => stack.busy[`svc:${props.service.name}`]);

const steps = ['building', 'starting', 'switching'] as const;
const stepIndex = computed(() => {
  const s = c.value.state;
  if (s === 'changed' || s === 'superseded') return -1;
  if (s === 'held') return 2;
  if (s === 'failed') return c.value.build_id ? 1 : 0;
  return steps.indexOf(s as (typeof steps)[number]);
});

// A candidate has a process of its own from the health check on (and a
// failed one may have printed why it did not come up).
const hasProcess = computed(() => !!c.value.build_id && ['starting', 'switching', 'failed'].includes(c.value.state));
const showConsole = ref(false);
const consolePath = computed(() =>
  showConsole.value && hasProcess.value && stack.id && stack.running ? paths.api(stack.id, v1.logStream(props.service.name, 500, c.value.build_id!)) : null,
);

const heldParts = computed(() => (c.value.reason ?? '').split(/(devstack promote \S+)/g).filter(Boolean));
</script>

<template>
  <section class="cand" :class="`tone-${look.tone}`" data-testid="candidate-banner" :data-state="c.state" aria-live="polite">
    <div class="cand-head">
      <div class="cand-icon" aria-hidden="true">
        <span v-if="look.busy" class="spinner" />
        <Icon v-else :name="look.icon" :size="18" />
      </div>
      <div class="grow">
        <div class="cand-title">
          <span class="eyebrow">Not yet active</span>
          <span class="state">{{ look.label }}</span>
          <span v-if="c.build_id" class="tag" :title="c.build_id">build {{ shortBuild(c.build_id) }}</span>
          <span class="muted since">· {{ ago(c.since, now) }}</span>
        </div>
        <p class="explain">{{ look.explain }}</p>
      </div>
      <div v-if="!readonly" class="cand-actions">
        <button
          v-if="hasProcess"
          type="button"
          class="btn"
          :aria-pressed="showConsole"
          data-testid="action-candidate-console"
          title="Output of the candidate's process"
          @click="showConsole = !showConsole"
        >
          <Icon name="terminal" /> Console
        </button>
        <button
          v-if="c.state === 'held'"
          type="button"
          class="btn btn-warn"
          :disabled="!!busy || stack.stale"
          data-testid="action-promote"
          @click="stack.serviceAction(service.name, 'promote')"
        >
          <span v-if="busy === 'promote'" class="spinner" /><Icon v-else name="promote" />
          Promote
        </button>
        <template v-if="c.state === 'failed'">
          <button type="button" class="btn" data-testid="action-build-log" @click="emit('buildLog')"><Icon name="file" /> Build log</button>
          <button
            type="button"
            class="btn"
            :disabled="!!busy || stack.stale"
            data-testid="action-retry"
            title="Build the current tree again"
            @click="stack.serviceAction(service.name, 'restart')"
          >
            <span v-if="busy === 'restart'" class="spinner" /><Icon v-else name="restart" />
            Rebuild
          </button>
        </template>
      </div>
    </div>

    <ol v-if="c.state !== 'changed' && c.state !== 'superseded'" class="steps" aria-label="Progress">
      <li v-for="(s, i) in steps" :key="s" :class="{ done: i < stepIndex, current: i === stepIndex && c.state !== 'held' && c.state !== 'failed', bad: c.state === 'failed' && i === stepIndex }">
        <span class="bullet" aria-hidden="true" />{{ s === 'building' ? 'Build' : s === 'starting' ? 'Health check' : 'Switch' }}
      </li>
      <li v-if="c.state === 'held'" class="current held"><span class="bullet" aria-hidden="true" />Waiting for promote</li>
    </ol>

    <div v-if="consolePath" class="cand-console">
      <LogView :path="consolePath" height="260px" empty-text="The candidate has printed nothing yet." testid="candidate-log" />
    </div>
    <div v-if="c.state === 'failed' && c.reason" class="cand-reason">
      <Diagnostics :text="c.reason" max-height="260px" />
    </div>
    <div v-else-if="c.state === 'held' && c.reason" class="cand-reason held-reason">
      <Icon name="database" :size="16" />
      <p>
        <template v-for="(p, i) in heldParts" :key="i"><code v-if="p.startsWith('devstack promote')">{{ p }}</code><template v-else>{{ p }}</template></template>
      </p>
    </div>
    <p v-else-if="c.reason" class="cand-reason muted">{{ c.reason }}</p>
  </section>
</template>

<style scoped>
.cand-console {
  border: 1px solid var(--border);
  border-radius: var(--radius);
  overflow: hidden;
  background: var(--surface);
}
.cand {
  --tone: var(--accent);
  --tone-soft: var(--accent-soft);
  --tone-text: var(--accent-text);
  border: 1px solid color-mix(in srgb, var(--tone) 40%, var(--border));
  background: linear-gradient(180deg, color-mix(in srgb, var(--tone-soft) 85%, transparent), var(--surface) 140px);
  border-radius: var(--radius-lg);
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  box-shadow: var(--shadow-sm);
}
.tone-warn {
  --tone: var(--warn);
  --tone-soft: var(--warn-soft);
  --tone-text: var(--warn-text);
}
.tone-danger {
  --tone: var(--danger);
  --tone-soft: var(--danger-soft);
  --tone-text: var(--danger-text);
}
.tone-info {
  --tone: var(--info);
  --tone-soft: var(--info-soft);
  --tone-text: var(--info-text);
}
.tone-neutral {
  --tone: var(--text-faint);
  --tone-soft: var(--neutral-soft);
  --tone-text: var(--neutral-text);
}
.cand-head {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  flex-wrap: wrap;
}
.cand-head > .grow {
  flex: 1 1 0;
  min-width: 0;
}
@media (max-width: 560px) {
  .cand-actions {
    flex-basis: 100%;
    margin-left: 46px;
  }
}
.cand-icon {
  width: 34px;
  height: 34px;
  border-radius: 10px;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--tone) 18%, transparent);
  color: var(--tone-text);
  flex: none;
}
.cand-title {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.eyebrow {
  font-size: 11px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--tone-text);
}
.state {
  font-size: 16px;
  font-weight: 650;
}
.since {
  font-size: 12.5px;
}
.explain {
  color: var(--text-2);
  font-size: 13px;
  margin-top: 2px;
}
.cand-actions {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  margin-left: auto;
}
.steps {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  gap: 4px 18px;
  flex-wrap: wrap;
  font-size: 12.5px;
  color: var(--text-muted);
}
.steps li {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.bullet {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  border: 2px solid var(--border-strong);
}
.steps li.done {
  color: var(--text-2);
}
.steps li.done .bullet {
  background: var(--ok);
  border-color: var(--ok);
}
.steps li.current {
  color: var(--tone-text);
  font-weight: 600;
}
.steps li.current .bullet {
  border-color: var(--tone);
  background: color-mix(in srgb, var(--tone) 40%, transparent);
  animation: pulse 1.4s ease-in-out infinite;
}
.steps li.bad,
.steps li.held {
  color: var(--tone-text);
  font-weight: 600;
}
.steps li.bad .bullet,
.steps li.held .bullet {
  animation: none;
  border-color: var(--tone);
  background: var(--tone);
}
.held-reason {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  padding: 10px 12px;
  border-radius: var(--radius);
  background: color-mix(in srgb, var(--warn-soft) 70%, var(--surface));
  border: 1px dashed color-mix(in srgb, var(--warn) 50%, transparent);
  font-size: 13px;
  color: var(--text);
}
.held-reason svg {
  color: var(--warn-text);
  margin-top: 2px;
  flex: none;
}
.held-reason p {
  overflow-wrap: anywhere;
}
.held-reason code {
  font-weight: 600;
}
</style>
