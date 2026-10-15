<script setup lang="ts">
import { ref } from 'vue';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';

const hub = useHub();
const checking = ref(false);

async function retry() {
  checking.value = true;
  try {
    await hub.checkSession();
    if (hub.status === 'ok') await hub.init();
  } finally {
    checking.value = false;
  }
}
</script>

<template>
  <main class="unpaired" data-testid="unpaired">
    <div class="card box">
      <div class="logo" aria-hidden="true"><Icon name="key" :size="26" /></div>
      <h1>This browser is not paired with devstack</h1>
      <p class="muted">
        The dashboard only talks to the devstack hub on this machine, and only from a browser that was paired with it. Pairing takes one command.
      </p>
      <ol class="steps">
        <li>
          <span class="n">1</span>
          <div>Open a terminal in your project (the folder with <code>devstack.toml</code>).</div>
        </li>
        <li>
          <span class="n">2</span>
          <div>
            Run
            <pre class="cmd"><code>devstack ui</code></pre>
            It starts the hub if needed and opens this page with a one-time code.
          </div>
        </li>
      </ol>
      <div class="row foot">
        <button type="button" class="btn btn-primary" :disabled="checking" @click="retry">
          <span v-if="checking" class="spinner" /><Icon v-else name="restart" /> Check again
        </button>
        <span class="faint small">The session is stored in an HttpOnly cookie for this hub only.</span>
      </div>
    </div>
  </main>
</template>

<style scoped>
.unpaired {
  min-height: 100vh;
  display: grid;
  place-items: center;
  padding: 24px 16px;
  background:
    radial-gradient(800px 400px at 20% -10%, color-mix(in srgb, var(--accent) 14%, transparent), transparent),
    radial-gradient(600px 300px at 110% 110%, color-mix(in srgb, var(--ok) 10%, transparent), transparent),
    var(--bg);
}
.box {
  width: min(560px, 100%);
  padding: 32px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  box-shadow: var(--shadow-lg);
}
@media (max-width: 480px) {
  .box {
    padding: 22px 18px;
  }
}
.logo {
  width: 52px;
  height: 52px;
  border-radius: 14px;
  display: grid;
  place-items: center;
  background: var(--accent-soft);
  color: var(--accent-text);
}
h1 {
  font-size: 22px;
}
.steps {
  list-style: none;
  padding: 0;
  margin: 4px 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.steps li {
  display: flex;
  gap: 12px;
}
.n {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--surface-3);
  display: grid;
  place-items: center;
  font-weight: 700;
  font-size: 12px;
  flex: none;
}
.cmd {
  margin: 8px 0;
  padding: 10px 14px;
  background: var(--log-bg);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  font-size: 14px;
}
.cmd code {
  background: none;
  padding: 0;
  font-size: 14px;
  font-weight: 600;
}
.foot {
  flex-wrap: wrap;
  gap: 12px;
}
.small {
  font-size: 12px;
}
</style>
