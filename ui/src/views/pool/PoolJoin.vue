<script setup lang="ts">
// Joining a pool from here, as `devstack join` does; with `another`, beside
// this machine's other pools under a name of its own (ADR-0034).
import { computed, ref } from 'vue';
import { useRouter } from 'vue-router';
import Icon from '@/components/icons/Icon.vue';
import { usePool } from '@/stores/pool';

const props = defineProps<{ another?: boolean }>();
const pool = usePool();
const router = useRouter();

const url = ref('');
const fp = ref('');
const name = ref('');
const as = ref('');
const joining = ref(false);
// `this-machine` is the pool area's own page, not a pool's name.
const asOk = computed(() => /^[a-z0-9_-]{1,32}$/.test(as.value.trim()) && as.value.trim() !== 'this-machine' && !pool.pools.some((p) => p.name === as.value.trim() && p.joined));
async function join() {
  joining.value = true;
  const here = props.another ? as.value.trim() : pool.current;
  if (await pool.join(url.value.trim(), fp.value.trim(), name.value.trim(), here)) void router.push({ name: 'pool-overview', params: { pool: here } });
  joining.value = false;
}
</script>

<template>
  <section class="card" :data-testid="another ? 'pool-join-another' : undefined">
    <header class="card-header">
      <Icon name="key" :size="18" />
      <h2>{{ another ? 'Join another pool' : 'Join a pool' }}</h2>
    </header>
    <div class="card-body">
      <template v-if="another">
        <p class="muted small">
          A pool of another organisation (a customer's servers, say), beside this machine's other pools. Each pool decides alone whom it
          admits; they do not trust each other. The name is yours: it is how lanes name it (<code>--on name:node</code>).
        </p>
      </template>
      <template v-else>
        <p>This machine is in no pool. A pool lets machines run lanes for each other (remote lanes).</p>
        <p class="muted small">
          A member of the pool copies its join command from this page (or ask its admin for the coordinator's address and
          fingerprint). The fingerprint proves the coordinator is the one it claims; an admin then approves this machine.
        </p>
      </template>
      <form class="join" :data-testid="another ? undefined : 'pool-join'" @submit.prevent="join">
        <label v-if="another">
          <span>Name here</span>
          <input v-model="as" class="input" placeholder="customer" required data-testid="pool-join-name" />
        </label>
        <label>
          <span>Coordinator</span>
          <input v-model="url" class="input" placeholder="https://10.0.0.5:4260" required data-testid="pool-join-url" />
        </label>
        <label class="wide">
          <span>Fingerprint</span>
          <input v-model="fp" class="input mono" placeholder="64 hex digits" required data-testid="pool-join-fp" />
        </label>
        <label>
          <span>This machine's name (optional)</span>
          <input v-model="name" class="input" placeholder="this machine's name" />
        </label>
        <button type="submit" class="btn btn-primary btn-sm" :disabled="joining || !url || !fp || (another && !asOk)" data-testid="pool-join-submit">
          <span v-if="joining" class="spinner" /><Icon v-else name="key" :size="14" /> Ask to join
        </button>
        <RouterLink v-if="another" :to="{ query: {} }" class="btn btn-sm btn-ghost">Cancel</RouterLink>
      </form>
    </div>
  </section>
</template>

<style scoped>
.join {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 10px;
  margin-top: 10px;
}
.join label {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--text-muted);
}
.join .wide input {
  width: 420px;
  max-width: 100%;
}
.small {
  font-size: 12px;
}
</style>
