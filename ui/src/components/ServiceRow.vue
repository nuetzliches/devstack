<script setup lang="ts">
import { computed } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { ServiceView } from '@/api/types';
import { ago, serviceUrl, shortBuild, useNow } from '@/lib/format';
import { candidateLook, candidateSummary, kindIcon, kindLabel, rebuildLook, rebuildSummary, statusLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import StatusPill from './StatusPill.vue';
import ErrorLine from './ErrorLine.vue';
import RowUsage from './RowUsage.vue';
import SplitButton from './SplitButton.vue';
import type { MenuItem } from '@/lib/laneActions';
import { usePins } from '@/lib/pins';
import Icon from './icons/Icon.vue';

const props = defineProps<{ service: ServiceView }>();
const stack = useStack();
const pins = usePins();
const pinned = computed(() => pins.has(stack.id, props.service.name));
const load = computed(() => stack.usage?.services.find((u) => u.name === props.service.name) ?? null);
const now = useNow();

const look = computed(() => statusLook(props.service.status));
const cand = computed(() => props.service.candidate);
const candLook = computed(() => (cand.value ? candidateLook(cand.value) : null));
// A dev server's own rebuild (ADR-0042), where a built service shows its candidate.
const reb = computed(() => (cand.value ? null : (props.service.rebuild ?? null)));
const rebLook = computed(() => (reb.value ? rebuildLook(reb.value) : null));
const busy = computed(() => stack.busy[`svc:${props.service.name}`]);
/** Restart's menu: Rebuild, for a service that runs a build. */
const restartItems = computed<MenuItem[]>(() => [
  {
    key: 'rebuild',
    label: 'Rebuild',
    icon: 'wrench',
    hint: 'built again',
    disabled: !props.service.build_id,
    title: props.service.build_id ? `Build ${props.service.name} again, even for inputs built before, and restart it` : `${props.service.name} runs no build`,
  },
]);
const isStopped = computed(() => ['stopped', 'completed', 'exited', 'failed'].includes(props.service.status.state));
const to = computed(() => ({ name: 'service', params: { stack: stack.id ?? '', name: props.service.name } }));
const route = useRoute();
const router = useRouter();
const selected = computed(() => route.name === 'service' && route.params.name === props.service.name);
/** A click anywhere on the row but its buttons and links opens the detail. */
function open(e: MouseEvent) {
  if ((e.target as Element).closest('button, a, input, label')) return;
  void router.push(to.value);
}
</script>

<template>
  <li class="svc-row" :class="{ 'has-cand': !!cand, selected }" :aria-current="selected ? 'true' : undefined" @click="open" data-testid="service-row" :data-service="service.name" :data-state="service.status.state">
    <div class="main">
      <span class="kind" :title="kindLabel(service)"><Icon :name="kindIcon(service)" :size="15" /></span>
      <div class="grow name-col">
        <RouterLink :to="to" class="name" data-testid="service-link">{{ service.name }}</RouterLink>
        <div class="meta">
          <span v-if="service.hot_reload" class="tag hot" title="hot-reload mode: the toolchain's watcher runs from the source tree" data-testid="service-hot-reload">hot reload</span>
          <span v-if="service.build_id" class="tag" :title="`running build ${service.build_id}`" data-testid="service-build">{{ shortBuild(service.build_id) }}</span>
          <a v-if="service.port" class="m port-link" :href="serviceUrl(service)" @click="stack.followLink($event, serviceUrl(service))" @auxclick="stack.followLink($event, serviceUrl(service))" target="_blank" rel="noopener" :title="`open ${serviceUrl(service)}`" data-testid="service-url"><Icon name="link" :size="12" />:{{ service.port }}</a>
          <span v-if="service.pid" class="m hide-sm">pid {{ service.pid }}</span>
          <span v-if="service.kind !== 'service'" class="m">{{ kindLabel(service) }}</span>
        </div>
      </div>
      <RowUsage :series-key="`svc:${service.name}`" :load="load" />
      <button
        type="button"
        class="btn btn-ghost btn-sm btn-icon pin"
        :class="{ on: pinned }"
        :aria-pressed="pinned"
        :aria-label="pinned ? `Unpin ${service.name}` : `Pin ${service.name}`"
        :title="pinned ? 'Unpin' : 'Pin to the top (this browser only)'"
        data-testid="action-pin"
        @click="pins.toggle(stack.id, service.name)"
      >
        <Icon name="pin" :size="14" />
      </button>
      <StatusPill class="col-pill" :tone="look.tone" :label="look.label" :busy="look.busy" :title="look.detail ?? `since ${ago(service.since, now)}`" />
      <div v-if="!stack.readOnly" class="col-actions">
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
        <SplitButton
          v-else
          look="ghost"
          icon="restart"
          :title="`Restart ${service.name}: from the build there is for its inputs, else built; switches when healthy`"
          :items="restartItems"
          :disabled="!!busy || stack.stale"
          :busy="!!busy"
          :menu-label="`More restarts of ${service.name}`"
          testid="action-restart"
          @main="stack.serviceAction(service.name, 'restart')"
          @pick="stack.serviceAction(service.name, 'rebuild')"
        />
        <button
          v-if="!isStopped"
          type="button"
          class="btn btn-ghost btn-sm btn-icon btn-stop"
          :aria-label="`Stop ${service.name}`"
          title="Stop"
          :disabled="!!busy || stack.stale"
          data-testid="action-stop"
          @click="stack.serviceAction(service.name, 'stop')"
        >
          <Icon name="stop" />
        </button>
      </div>
    </div>
    <ErrorLine v-if="look.detail && !cand" class="detail" :text="look.detail" :tone="look.tone === 'danger' ? 'danger' : 'warn'" />
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
    <RouterLink v-else-if="reb && rebLook" :to="to" class="cand-line" :class="`tone-${rebLook.tone === 'ok' ? 'neutral' : rebLook.tone}`" data-testid="rebuild-line" :data-state="reb.state">
      <span v-if="rebLook.busy" class="spinner sm" aria-hidden="true" />
      <Icon v-else :name="rebLook.icon" :size="14" />
      <span class="cl-label">Dev server:</span>
      <span class="cl-state">{{ rebuildSummary(reb) }}</span>
      <span v-if="reb.state === 'failed' && reb.errors?.length" class="cl-err" :title="reb.errors.join('\n')">{{ reb.errors[0] }}</span>
      <span class="grow" />
      <span class="cl-ago">{{ ago(reb.since, now) }}</span>
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
.svc-row {
  cursor: pointer;
}
.svc-row:hover {
  background: var(--surface-hover);
}
.svc-row.selected {
  background: var(--accent-soft);
  box-shadow: inset 3px 0 0 var(--accent);
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
  white-space: nowrap;
}
.port-link {
  color: inherit;
  text-decoration: none;
}
.port-link:hover {
  color: var(--accent, inherit);
  text-decoration: underline;
}
.detail {
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
.cl-err {
  font-family: var(--font-mono);
  font-size: 11.5px;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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
.pin {
  color: var(--text-faint);
}
.pin.on {
  color: var(--accent);
}
</style>
