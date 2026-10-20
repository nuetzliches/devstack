<script setup lang="ts">
// The detail pane of the overview when nothing is picked: what needs
// attention (held or failed candidates, Compose errors, containers Docker
// lost the port forward of), changes on their way, else a hint.
import { computed, ref } from 'vue';
import { useStack } from '@/stores/stack';
import CandidatePanel from '@/components/CandidatePanel.vue';
import BuildLogModal from '@/components/BuildLogModal.vue';
import ErrorLine from '@/components/ErrorLine.vue';
import Icon from '@/components/icons/Icon.vue';

const stack = useStack();
const buildLogFor = ref<string | null>(null);
const repairing = ref(false);

const attention = computed(() => stack.pending.filter((s) => s.candidate && (s.candidate.state === 'held' || s.candidate.state === 'failed')));
const inFlight = computed(() => stack.pending.length - attention.value.length);
const infraErrors = computed(() => stack.view?.infra.filter((g) => g.error) ?? []);
/** Running containers this machine cannot reach (Docker Desktop lost the forward). */
const unreachable = computed(() =>
  (stack.view?.infra ?? []).flatMap((g) => g.containers.filter((c) => c.unreachable?.length && c.managed !== false).map((c) => ({ group: g.group, service: c.service, ports: c.unreachable! }))),
);

async function repairAll() {
  repairing.value = true;
  try {
    for (const c of unreachable.value) await stack.containerAction(c.group, c.service, 'restart');
  } finally {
    repairing.value = false;
  }
}
</script>

<template>
  <div class="pane" data-testid="overview-home">
    <div v-for="g in infraErrors" :key="g.group" class="banner danger">
      <Icon name="alert" :size="18" />
      <strong>Compose group {{ g.group }}:</strong>
      <ErrorLine class="grow" :text="g.error ?? ''" tone="danger" />
    </div>

    <div v-if="unreachable.length" class="banner warn" data-testid="lost-forwards">
      <Icon name="wifiOff" :size="18" />
      <span class="grow">
        <strong>{{ unreachable.length }} container{{ unreachable.length === 1 ? '' : 's' }} not reachable from this machine</strong>
        ({{ unreachable.map((c) => c.service).join(', ') }}): Docker lost the port forward. A restart repairs it.
      </span>
      <button v-if="!stack.readOnly" type="button" class="btn btn-sm" :disabled="repairing || stack.stale" data-testid="action-repair-forwards" @click="repairAll">
        <span v-if="repairing" class="spinner" /><Icon v-else name="restart" :size="14" /> Restart {{ unreachable.length === 1 ? 'it' : 'them' }}
      </button>
    </div>

    <section v-if="attention.length" class="attention" aria-label="Needs attention">
      <h2 class="section-title">Needs attention</h2>
      <div v-for="s in attention" :key="s.name" class="att-item">
        <RouterLink :to="{ name: 'service', params: { stack: stack.id ?? '', name: s.name } }" class="att-name">{{ s.name }} <Icon name="chevronRight" :size="14" /></RouterLink>
        <CandidatePanel :service="s" @build-log="buildLogFor = s.name" />
      </div>
    </section>
    <p v-if="inFlight" class="inflight muted"><span class="spinner" /> {{ inFlight }} change{{ inFlight === 1 ? '' : 's' }} on the way; the running instances keep serving until the new builds are healthy.</p>

    <div v-if="!attention.length && !infraErrors.length && !unreachable.length" class="card">
      <div class="empty">
        <Icon name="layers" :size="32" />
        <h2>Nothing needs you right now</h2>
        <p>Pick a service or container on the left for its details and its log.</p>
      </div>
    </div>

    <BuildLogModal v-if="buildLogFor && stack.id" :open="!!buildLogFor" :stack-id="stack.id" :lane="stack.apiLane" :service="buildLogFor" @close="buildLogFor = null" />
  </div>
</template>

<style scoped>
.pane {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}
.banner {
  flex-wrap: wrap;
}
.attention {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.att-item {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.att-name {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-weight: 600;
  align-self: flex-start;
}
.inflight {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  margin: 0;
}
</style>
