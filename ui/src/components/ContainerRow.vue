<script setup lang="ts">
import { computed } from 'vue';
import type { ContainerView } from '@/api/types';
import { containerLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import StatusPill from './StatusPill.vue';
import Icon from './icons/Icon.vue';

const props = withDefaults(defineProps<{ group: string; container: ContainerView | null; service: string; showGroup?: boolean }>(), { showGroup: false });
const stack = useStack();
const look = computed(() => (props.container ? containerLook(props.container) : null));
const busy = computed(() => !!stack.busy[`ctr:${props.group}/${props.service}`]);

/** "5432->127.0.0.1:15432" -> host side first, which is what people type. */
function hostPort(p: string): string {
  const m = /^(\d+)(?:\/\w+)?->(.+)$/.exec(p);
  return m ? `${m[2].replace(/^0\.0\.0\.0:|^127\.0\.0\.1:/, ':')} → ${m[1]}` : p;
}

/** The link of a published port: a URL naming its host port. */
function urlFor(p: string): string | undefined {
  const port = p.split(':').pop();
  return props.container?.urls?.find((u) => new RegExp(`:${port}(?:/|$)`).test(u));
}

/** A link the stack file set that names none of the ports (another path or host). */
const extraUrls = computed(() => (props.container?.urls ?? []).filter((u) => !props.container!.ports.some((p) => urlFor(p) === u)));
</script>

<template>
  <li class="ctr-row" data-testid="container-row" :data-container="`${group}/${service}`">
    <span class="kind" title="compose container"><Icon name="box" :size="15" /></span>
    <div class="grow name-col">
      <div class="name">
        {{ service }}<span v-if="showGroup" class="muted"> · {{ group }}</span>
      </div>
      <div class="meta">
        <template v-if="container">
          <template v-for="p in container.ports" :key="p">
            <a v-if="urlFor(p)" class="tag port-link" :href="urlFor(p)" target="_blank" rel="noopener" :title="`open ${urlFor(p)}`" data-testid="container-url">{{ hostPort(p) }}</a>
            <span v-else class="tag" :title="p">{{ hostPort(p) }}</span>
          </template>
          <a v-for="u in extraUrls" :key="u" class="tag port-link" :href="u" target="_blank" rel="noopener" data-testid="container-url">{{ u.replace(/^https?:\/\//, '') }}</a>
          <span v-if="!container.ports.length" class="faint">no published ports</span>
        </template>
        <span v-else class="faint">not created</span>
      </div>
    </div>
    <StatusPill v-if="look" :tone="look.tone" :label="look.label" :busy="look.busy" :title="container ? `${container.name}: ${container.state}${container.health ? ` (${container.health})` : ''}` : undefined" />
    <StatusPill v-else tone="neutral" label="Missing" />
    <button
      type="button"
      class="btn btn-ghost btn-sm btn-icon"
      :aria-label="`Restart container ${service}`"
      title="Restart container"
      :disabled="busy || stack.stale || !container"
      data-testid="action-container-restart"
      @click="stack.restartContainer(group, service)"
    >
      <span v-if="busy" class="spinner" /><Icon v-else name="restart" />
    </button>
  </li>
</template>

<style scoped>
.ctr-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 16px;
  border-bottom: 1px solid var(--border);
  min-width: 0;
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
.port-link {
  color: inherit;
  text-decoration: none;
}
.port-link:hover {
  color: var(--accent);
  text-decoration: underline;
}
</style>
