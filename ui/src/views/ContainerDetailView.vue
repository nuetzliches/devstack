<script setup lang="ts">
// A Compose container of the stack, as a page of its own like a service:
// state, ports, start/stop/restart, its load and its log.
import { computed } from 'vue';
import { useRoute } from 'vue-router';
import { bytes, percent } from '@/lib/format';
import { containerLook } from '@/lib/status';
import { useStack } from '@/stores/stack';
import StatusPill from '@/components/StatusPill.vue';
import ContainerLog from '@/components/ContainerLog.vue';
import Sparkline from '@/components/Sparkline.vue';
import Icon from '@/components/icons/Icon.vue';

const route = useRoute();
const stack = useStack();

const group = computed(() => route.params.group as string);
const service = computed(() => route.params.service as string);
const infra = computed(() => stack.view?.infra.find((g) => g.group === group.value) ?? null);
const container = computed(() => infra.value?.containers.find((c) => c.service === service.value) ?? null);
const look = computed(() => (container.value ? containerLook(container.value) : null));
const busy = computed(() => stack.busy[`ctr:${group.value}/${service.value}`]);
const running = computed(() => container.value?.state === 'running' || container.value?.state === 'restarting');

const series = computed(() => stack.history.get(`ctr:${group.value}/${service.value}`) ?? []);
const cpuValues = computed(() => series.value.map((p) => p.cpu));
const memValues = computed(() => series.value.map((p) => p.memory));
const current = computed(() => stack.usage?.containers.find((c) => c.group === group.value && c.service === service.value) ?? null);

/** "5432->127.0.0.1:15432" -> ":15432 → 5432". */
function hostPort(p: string): string {
  const m = /^(\d+)(?:\/\w+)?->(.+)$/.exec(p);
  return m ? `${m[2].replace(/^0\.0\.0\.0:|^127\.0\.0\.1:/, ':')} → ${m[1]}` : p;
}

const kv = computed(() => {
  const c = container.value;
  return [
    { k: 'Compose group', v: group.value },
    { k: 'Project', v: infra.value?.project ?? '—', mono: true },
    { k: 'Container', v: c?.name ?? 'not created', mono: !!c },
    { k: 'State', v: c ? `${c.state}${c.health ? ` (${c.health})` : ''}` : '—' },
    { k: 'Ports', v: c?.ports.length ? c.ports.map(hostPort).join(', ') : 'none published', mono: !!c?.ports.length },
  ];
});
</script>

<template>
  <div class="page">
    <RouterLink :to="{ name: 'infra', params: { stack: stack.id ?? '' } }" class="back"><Icon name="chevronLeft" :size="14" /> Infrastructure</RouterLink>

    <header class="page-header" data-testid="container-detail" :data-container="`${group}/${service}`">
      <div class="row head-left">
        <span class="kind"><Icon name="box" :size="20" /></span>
        <div>
          <div class="row wrap">
            <h1>{{ service }}</h1>
            <StatusPill v-if="look" :tone="look.tone" :label="look.label" :busy="look.busy" data-testid="container-status" />
            <StatusPill v-else tone="neutral" label="Missing" />
          </div>
          <div class="subtitle">
            container · {{ group }}
            <template v-for="u in container?.urls ?? []" :key="u">
              · <a :href="u" target="_blank" rel="noopener" data-testid="container-url">{{ u.replace(/^https?:\/\//, '') }} <Icon name="external" :size="12" /></a>
            </template>
          </div>
        </div>
      </div>
      <div class="btn-group">
        <button v-if="!running" type="button" class="btn btn-primary" :disabled="!!busy || stack.stale" data-testid="action-container-start" @click="stack.containerAction(group, service, 'start')">
          <span v-if="busy === 'start'" class="spinner" /><Icon v-else name="play" /> Start
        </button>
        <button v-if="running" type="button" class="btn" :disabled="!!busy || stack.stale" data-testid="action-container-restart" @click="stack.containerAction(group, service, 'restart')">
          <span v-if="busy === 'restart'" class="spinner" /><Icon v-else name="restart" /> Restart
        </button>
        <button v-if="running" type="button" class="btn" :disabled="!!busy || stack.stale" data-testid="action-container-stop" @click="stack.containerAction(group, service, 'stop')">
          <span v-if="busy === 'stop'" class="spinner" /><Icon v-else name="stop" /> Stop
        </button>
      </div>
    </header>

    <div v-if="container?.unreachable?.length" class="banner warn">
      <Icon name="alert" :size="18" />
      <span class="grow">{{ container.unreachable.join(', ') }} closes connections from this machine at once: Docker lost the port forward (restart the container), or nothing listens on it inside.</span>
    </div>

    <div class="cols">
      <section class="card">
        <header class="card-header"><h2>Details</h2></header>
        <div class="card-body">
          <dl class="kv">
            <template v-for="row in kv" :key="row.k">
              <dt>{{ row.k }}</dt>
              <dd :class="{ mono: row.mono }">{{ row.v }}</dd>
            </template>
          </dl>
        </div>
      </section>
      <section class="card">
        <header class="card-header">
          <h2>Resources</h2>
          <span class="count">last 10 min</span>
        </header>
        <div class="card-body res">
          <div class="metric">
            <div class="mh">
              <span class="ml"><Icon name="cpu" :size="14" /> CPU</span>
              <span class="mv tabular">{{ percent(current?.cpu) }}</span>
            </div>
            <Sparkline :values="cpuValues" :width="300" :height="44" :min-max="5" fluid color="var(--accent)" label="CPU history" />
          </div>
          <div class="metric">
            <div class="mh">
              <span class="ml"><Icon name="hardDrive" :size="14" /> Memory</span>
              <span class="mv tabular">{{ bytes(current?.memory) }}</span>
            </div>
            <Sparkline :values="memValues" :width="300" :height="44" :min-max="8 * 1024 * 1024" :zero-based="false" fluid color="var(--ok)" label="Memory history" />
          </div>
        </div>
      </section>
    </div>

    <ContainerLog v-if="stack.id" :stack-id="stack.id" :group="group" :service="service" />
  </div>
</template>

<style scoped>
.back {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 13px;
  color: var(--text-muted);
  align-self: flex-start;
}
.head-left {
  gap: 12px;
  align-items: flex-start;
  min-width: 0;
}
.kind {
  width: 40px;
  height: 40px;
  border-radius: 11px;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--info) 12%, transparent);
  color: var(--info-text);
  flex: none;
}
.subtitle a {
  display: inline-flex;
  align-items: center;
  gap: 3px;
}
.cols {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 16px;
  align-items: start;
}
@media (max-width: 900px) {
  .cols {
    grid-template-columns: minmax(0, 1fr);
  }
}
.res {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.mh {
  display: flex;
  justify-content: space-between;
  font-size: 13px;
  margin-bottom: 4px;
}
.ml {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  color: var(--text-muted);
}
.mv {
  font-weight: 600;
}
</style>
