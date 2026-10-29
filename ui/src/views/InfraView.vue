<script setup lang="ts">
import { computed } from 'vue';
import { bytes, percent } from '@/lib/format';
import { containerBucket } from '@/lib/status';
import { useStack } from '@/stores/stack';
import ContainerRow from '@/components/ContainerRow.vue';
import Icon from '@/components/icons/Icon.vue';

const stack = useStack();
const groups = computed(() => stack.view?.infra ?? []);

function usageOf(group: string) {
  const u = stack.usage;
  if (!u) return null;
  const cs = u.containers.filter((c) => c.group === group);
  if (!cs.length) return null;
  return { cpu: cs.reduce((a, c) => a + c.cpu, 0), memory: cs.reduce((a, c) => a + c.memory, 0) };
}

function summary(group: string) {
  const g = groups.value.find((x) => x.group === group);
  if (!g) return '';
  const ok = g.containers.filter((c) => containerBucket(c) === 'healthy').length;
  return `${ok}/${g.containers.length} up`;
}
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <h1>Infrastructure</h1>
        <p class="subtitle">Compose groups started by this stack. Containers stay up when services restart.</p>
      </div>
    </header>

    <div v-if="stack.usage?.docker_error" class="banner warn">
      <Icon name="alert" :size="18" />
      <span class="grow"><strong>Docker:</strong> {{ stack.usage.docker_error }}</span>
    </div>

    <!-- One group under the other, full width: a container row needs the width the overview gives it. -->
    <div v-if="groups.length" class="groups stale-dim">
      <section v-for="g in groups" :key="g.group" class="card" :data-testid="`infra-${g.group}`">
        <header class="card-header">
          <Icon name="layers" :size="16" class="muted" />
          <h2>{{ g.group }}</h2>
          <span class="tag" title="compose project">{{ g.project }}</span>
          <span class="grow" />
          <span class="count">{{ summary(g.group) }}</span>
        </header>
        <div v-if="g.error" class="card-body err-body">
          <div class="banner danger"><Icon name="alert" :size="16" /><span class="grow mono">{{ g.error }}</span></div>
        </div>
        <ul v-if="g.containers.length" class="list">
          <ContainerRow v-for="c in g.containers" :key="c.service" :group="g.group" :service="c.service" :container="c" />
        </ul>
        <div v-else class="empty"><p>No containers.</p></div>
        <footer v-if="usageOf(g.group)" class="card-foot muted">
          <span><Icon name="cpu" :size="13" /> {{ percent(usageOf(g.group)!.cpu) }}</span>
          <span><Icon name="memoryStick" :size="13" /> {{ bytes(usageOf(g.group)!.memory) }}</span>
        </footer>
      </section>
    </div>
    <div v-else class="card">
      <div class="empty">
        <Icon name="box" :size="32" />
        <h2>No infrastructure</h2>
        <p>This stack does not start any Compose groups.</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.groups {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}
.list {
  list-style: none;
  margin: 0;
  padding: 0;
}
.err-body {
  padding-bottom: 0;
}
.card-foot {
  display: flex;
  gap: 16px;
  padding: 8px 16px;
  border-top: 1px solid var(--border);
  font-size: 12px;
  background: var(--surface-2);
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
}
.card-foot span {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
</style>
