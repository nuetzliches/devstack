<script setup lang="ts">
// `/`: go to the last selected stack, else the first running one.
import { computed, ref, watchEffect } from 'vue';
import { useRouter } from 'vue-router';
import { persist } from '@/lib/persist';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';
import ScanStacks from '@/components/ScanStacks.vue';
import AddStackDialog from '@/components/AddStackDialog.vue';
import { stackPage } from '@/lib/stackRoutes';

const hub = useHub();
const router = useRouter();
const addOne = ref(false);

const target = computed(() => {
  const last = persist.lastStack();
  if (last && (hub.entry(last) || persist.stack(last))) return last;
  return hub.stacks.find((s) => s.running)?.id ?? hub.stacks[0]?.id ?? null;
});

watchEffect(() => {
  if (target.value) void router.replace(stackPage(target.value));
});
</script>

<template>
  <div class="page">
    <section v-if="!target" class="card welcome" data-testid="welcome">
      <header class="card-header">
        <Icon name="layers" :size="18" />
        <h1>Find your stacks</h1>
      </header>
      <div class="card-body">
        <p>
          Choose the folder your repositories live in (for example <code>D:\Sources</code>): devstack lists every repository with a
          <code>devstack.toml</code> and adds the ones you want. Starting them is one click afterwards.
        </p>
        <ScanStacks />
        <p class="muted alt">
          A single repository, or one without a <code>devstack.toml</code> yet (devstack can write a first draft):
          <button type="button" class="btn btn-sm" data-testid="welcome-add-one" @click="addOne = true"><Icon name="plus" :size="14" /> Add one repository</button>
          · or run <code>devstack up</code> in it.
        </p>
      </div>
      <AddStackDialog :open="addOne" @close="addOne = false" />
    </section>
  </div>
</template>

<style scoped>
.welcome {
  max-width: 820px;
  margin: 24px auto;
}
.welcome h1 {
  font-size: 18px;
  margin: 0;
}
.alt {
  margin-top: 16px;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
</style>
