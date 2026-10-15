<script setup lang="ts">
import { computed } from 'vue';
import type { ServiceView } from '@/api/types';
import { ago, serviceUrl, shortBuild, useNow } from '@/lib/format';
import { candidateLook, candidateSummary, kindIcon, kindLabel, statusLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import StatusPill from './StatusPill.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ service: ServiceView }>();
const stack = useStack();
const now = useNow();

const look = computed(() => statusLook(props.service.status));
const cand = computed(() => props.service.candidate);
const candLook = computed(() => (cand.value ? candidateLook(cand.value) : null));
const busy = computed(() => stack.busy[`svc:${props.service.name}`]);
const isStopped = computed(() => ['stopped', 'completed', 'exited', 'failed'].includes(props.service.status.state));
const to = computed(() => ({ name: 'service', params: { stack: stack.id ?? '', name: props.service.name } }));
</script>

<template>
  <li class="svc-row" :class="{ 'has-cand': !!cand }" data-testid="service-row" :data-service="service.name" :data-state="service.status.state">
    <div class="main">
      <span class="kind" :title="kindLabel(service)"><Icon :name="kindIcon(service)" :size="15" /></span>
      <div class="grow name-col">
        <RouterLink :to="to" class="name" data-testid="service-link">{{ service.name }}</RouterLink>
        <div class="meta">
          <span v-if="service.build_id" class="tag" :title="`running build ${service.build_id}`" data-testid="service-build">{{ shortBuild(service.build_id) }}</span>
          <a v-if="service.port" class="m port-link" :href="serviceUrl(service)" target="_blank" rel="noopener" :title="`open ${serviceUrl(service)}`" data-testid="service-url"><Icon name="link" :size="12" />:{{ service.port }}</a>
          <span v-if="service.pid" class="m hide-sm">pid {{ service.pid }}</span>
          <span v-if="service.kind !== 'service'" class="m">{{ kindLabel(service) }}</span>
        </div>
      </div>
      <StatusPill :tone="look.tone" :label="look.label" :busy="look.busy" :title="look.detail ?? `since ${ago(service.since, now)}`" />
      <div class="actions">
        <button
          v-if="isStopped"
          type="button"
          class="btn btn-ghost btn-sm btn-icon"
          :aria-label="`Start ${service.name}`"
          title="Start"
          :disabled="!!busy || stack.stale"
          data-testid="action-start"
          @click="stack.serviceAction(service.name, 'start')"
        >
          <span v-if="busy" class="spinner" /><Icon v-else name="play" />
        </button>
        <button
          v-else
          type="button"
          class="btn btn-ghost btn-sm btn-icon"
          :aria-label="`Restart ${service.name}`"
          title="Restart (rebuild, switch when healthy)"
          :disabled="!!busy || stack.stale"
          data-testid="action-restart"
          @click="stack.serviceAction(service.name, 'restart')"
        >
          <span v-if="busy" class="spinner" /><Icon v-else name="restart" />
        </button>
      </div>
    </div>
    <p v-if="look.detail && !cand" class="detail">{{ look.detail }}</p>
    <RouterLink v-if="cand && candLook" :to="to" class="cand-line" :class="`tone-${candLook.tone}`" data-testid="candidate-line" :data-state="cand.state">
      <span v-if="candLook.busy" class="spinner sm" aria-hidden="true" />
      <Icon v-else :name="candLook.icon" :size="14" />
      <span class="cl-label">Not yet active:</span>
      <span class="cl-state">{{ candidateSummary(cand) }}</span>
      <span v-if="cand.build_id" class="cl-build">→ {{ shortBuild(cand.build_id) }}</span>
      <span class="grow" />
      <span class="cl-ago">{{ ago(cand.since, now) }}</span>
      <Icon name="chevronRight" :size="14" />
    </RouterLink>
  </li>
</template>

<style scoped>
.svc-row {
  padding: 10px 16px;
  border-bottom: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.svc-row:last-child {
  border-bottom: 0;
}
.main {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}
.kind {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  background: var(--surface-3);
  color: var(--text-muted);
  flex: none;
}
.name-col {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.name {
  font-weight: 600;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.meta {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text-muted);
  flex-wrap: wrap;
  margin-top: 1px;
}
.meta .m {
  display: inline-flex;
  align-items: center;
  gap: 2px;
}
.port-link {
  color: inherit;
  text-decoration: none;
}
.port-link:hover {
  color: var(--accent, inherit);
  text-decoration: underline;
}
.actions {
  display: flex;
  gap: 2px;
}
.detail {
  font-size: 12px;
  color: var(--warn-text);
  padding-left: 38px;
}
.cand-line {
  --tone: var(--accent);
  --tone-text: var(--accent-text);
  margin-left: 38px;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 5px 10px;
  border-radius: 8px;
  font-size: 12.5px;
  color: var(--tone-text);
  background: color-mix(in srgb, var(--tone) 11%, transparent);
  border: 1px solid color-mix(in srgb, var(--tone) 28%, transparent);
  text-decoration: none;
  min-width: 0;
}
.cand-line:hover {
  text-decoration: none;
  background: color-mix(in srgb, var(--tone) 17%, transparent);
}
.cand-line.tone-warn {
  --tone: var(--warn);
  --tone-text: var(--warn-text);
}
.cand-line.tone-danger {
  --tone: var(--danger);
  --tone-text: var(--danger-text);
}
.cand-line.tone-info {
  --tone: var(--info);
  --tone-text: var(--info-text);
}
.cand-line.tone-neutral {
  --tone: var(--text-faint);
  --tone-text: var(--text-muted);
}
.cl-label {
  font-weight: 700;
  white-space: nowrap;
}
.cl-state {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.cl-build {
  font-family: var(--font-mono);
  font-size: 11.5px;
}
.cl-ago {
  font-size: 11.5px;
  opacity: 0.8;
  white-space: nowrap;
}
.spinner.sm {
  width: 12px;
  height: 12px;
}
@media (max-width: 520px) {
  .hide-sm {
    display: none;
  }
  .cand-line,
  .detail {
    margin-left: 0;
    padding-left: 10px;
  }
  .cl-build {
    display: none;
  }
}
</style>
