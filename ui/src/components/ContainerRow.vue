<script setup lang="ts">
import { computed } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { ContainerView } from '@/api/types';
import { containerLook, containerRefuses } from '@/lib/status';
import { webUrl } from '@/lib/format';
import { notForwardedText } from '@/lib/lanes';
import { useStack } from '@/stores/stack';
import StatusPill from './StatusPill.vue';
import Icon from './icons/Icon.vue';
import RowUsage from './RowUsage.vue';
import { usePins } from '@/lib/pins';

const props = withDefaults(defineProps<{ group: string; container: ContainerView | null; service: string; showGroup?: boolean }>(), { showGroup: false });
const stack = useStack();
const pins = usePins();
const pinKey = computed(() => `compose.${props.group}.${props.service}`);
const pinned = computed(() => pins.has(stack.id, pinKey.value));
const load = computed(() => stack.usage?.containers.find((u) => u.group === props.group && u.service === props.service) ?? null);
const look = computed(() => (props.container ? containerLook(props.container) : null));
const busy = computed(() => !!stack.busy[`ctr:${props.group}/${props.service}`]);
const running = computed(() => props.container?.state === 'running' || props.container?.state === 'restarting');
/** Not one the group starts: the daemon stops it, but refuses start and restart. */
const refuses = computed(() => containerRefuses(props.container));
const to = computed(() => ({ name: 'container', params: { stack: stack.id ?? '', group: props.group, service: props.service } }));
const route = useRoute();
const router = useRouter();
const selected = computed(() => route.name === 'container' && route.params.group === props.group && route.params.service === props.service);
function open(e: MouseEvent) {
  if ((e.target as Element).closest('button, a, input, label')) return;
  void router.push(to.value);
}

/** "5432->127.0.0.1:15432" -> host side first, which is what people type. */
function hostPort(p: string): string {
  const m = /^(\d+)(?:\/\w+)?->(.+)$/.exec(p);
  return m ? `${m[2].replace(/^0\.0\.0\.0:|^127\.0\.0\.1:/, ':')} → ${m[1]}` : p;
}

/** The link of a published port: a URL naming its host port. */
function urlFor(p: string): string | undefined {
  const port = p.split(':').pop();
  return webUrls.value.find((u) => new RegExp(`:${port}(?:/|$)`).test(u));
}

/** The container's links that are web addresses; nothing else becomes a link. */
const webUrls = computed(() => (props.container?.urls ?? []).filter((u) => webUrl(u) !== undefined));

/** A link the stack file set that names none of the ports (another path or host). */
const extraUrls = computed(() => webUrls.value.filter((u) => !props.container!.ports.some((p) => urlFor(p) === u)));
</script>

<template>
  <li class="ctr-row" :class="{ selected }" :aria-current="selected ? 'true' : undefined" data-testid="container-row" @click="open" :data-container="`${group}/${service}`">
    <span class="kind" title="compose container"><Icon name="box" :size="15" /></span>
    <div class="grow name-col">
      <div class="name">
        <RouterLink :to="to" class="name-link" data-testid="container-link">{{ service }}</RouterLink><span v-if="showGroup" class="muted"> · {{ group }}</span>
      </div>
      <div class="meta">
        <template v-if="container">
          <template v-for="p in container.ports" :key="p">
            <a v-if="urlFor(p)" class="tag port-link" :href="urlFor(p)" @click="stack.followLink($event, urlFor(p)!)" @auxclick="stack.followLink($event, urlFor(p)!)" target="_blank" rel="noopener" :title="`open ${urlFor(p)}`" data-testid="container-url">{{ hostPort(p) }}</a>
            <span v-else class="tag" :title="p">{{ hostPort(p) }}</span>
          </template>
          <a v-for="u in extraUrls" :key="u" class="tag port-link" :href="u" @click="stack.followLink($event, u)" @auxclick="stack.followLink($event, u)" target="_blank" rel="noopener" data-testid="container-url">{{ u.replace(/^https?:\/\//, '') }}</a>
          <span v-if="!container.ports.length" class="faint">no published ports</span>
          <span v-if="refuses" class="tag" :title="refuses" data-testid="container-unmanaged">not in the stack file</span>
          <span v-if="container.not_forwarded?.length" class="tag" :title="notForwardedText(container.not_forwarded)" data-testid="container-not-forwarded">{{ container.not_forwarded.map((p) => p.toUpperCase()).join(', ') }} not forwarded</span>
          <button
            v-if="container.unreachable?.length && !refuses && !stack.readOnly"
            type="button"
            class="tag lost"
            :disabled="busy || stack.stale"
            :title="`${container.unreachable.join(', ')} closes connections from this machine at once: Docker lost the port forward, or nothing listens on it inside. Click to restart the container.`"
            data-testid="container-unreachable"
            @click="stack.containerAction(group, service, 'restart')"
          >
            not reachable from here · restart
          </button>
        </template>
        <span v-else class="faint">not created</span>
      </div>
    </div>
    <RowUsage :series-key="`ctr:${group}/${service}`" :load="load" />
    <button
      type="button"
      class="btn btn-ghost btn-sm btn-icon pin"
      :class="{ on: pinned }"
      :aria-pressed="pinned"
      :aria-label="pinned ? `Unpin ${service}` : `Pin ${service}`"
      :title="pinned ? 'Unpin' : 'Pin to the top (this browser only)'"
      data-testid="action-pin"
      @click="pins.toggle(stack.id, pinKey)"
    >
      <Icon name="pin" :size="14" />
    </button>
    <StatusPill class="row-pill" v-if="look" :tone="look.tone" :label="look.label" :busy="look.busy" :title="container ? `${container.name}: ${container.state}${container.health ? ` (${container.health})` : ''}` : undefined" />
    <StatusPill v-else class="row-pill" tone="neutral" label="Missing" />
    <!-- Someone else's lane (ADR-0040): nothing that would change it. -->
    <template v-if="stack.readOnly" />
    <button
      v-else-if="!running"
      type="button"
      class="btn btn-ghost btn-sm btn-icon"
      :aria-label="`Start container ${service}`"
      :title="refuses ?? 'Start container'"
      :disabled="busy || stack.stale || !!refuses"
      data-testid="action-container-start"
      @click="stack.containerAction(group, service, 'start')"
    >
      <span v-if="busy" class="spinner" /><Icon v-else name="play" />
    </button>
    <template v-else>
      <button
        type="button"
        class="btn btn-ghost btn-sm btn-icon"
        :aria-label="`Restart container ${service}`"
        :title="refuses ?? 'Restart container'"
        :disabled="busy || stack.stale || !!refuses"
        data-testid="action-container-restart"
        @click="stack.containerAction(group, service, 'restart')"
      >
        <span v-if="busy" class="spinner" /><Icon v-else name="restart" />
      </button>
      <button
        type="button"
        class="btn btn-ghost btn-sm btn-icon btn-stop"
        :aria-label="`Stop container ${service}`"
        title="Stop container"
        :disabled="busy || stack.stale"
        data-testid="action-container-stop"
        @click="stack.containerAction(group, service, 'stop')"
      >
        <Icon name="stop" />
      </button>
    </template>
  </li>
</template>

<style scoped>
.lost {
  color: var(--danger);
  border-color: var(--danger);
}
.ctr-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 16px;
  border-bottom: 1px solid var(--border);
  min-width: 0;
}
.ctr-row {
  cursor: pointer;
}
.ctr-row:hover {
  background: var(--surface-hover);
}
.ctr-row.selected {
  background: var(--accent-soft);
  box-shadow: inset 3px 0 0 var(--accent);
}
.lost {
  cursor: pointer;
  background: transparent;
  font: inherit;
}
.ctr-row:last-child {
  border-bottom: 0;
}
.kind {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--info) 12%, transparent);
  color: var(--info-text);
  flex: none;
}
.name-col {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.name {
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.meta {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  font-size: 12px;
  margin-top: 2px;
}
.name-link {
  color: inherit;
  text-decoration: none;
}
.name-link:hover {
  color: var(--accent);
  text-decoration: underline;
}
.port-link {
  color: inherit;
  text-decoration: none;
}
.port-link:hover {
  color: var(--accent);
  text-decoration: underline;
}
.pin {
  color: var(--text-faint);
}
.pin.on {
  color: var(--accent);
}
.row-pill {
  min-width: 88px;
  justify-content: center;
}
</style>
