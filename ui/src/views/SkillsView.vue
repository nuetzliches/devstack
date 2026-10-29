<script setup lang="ts">
// Skills (ADR-0074): the skill sets this machine's pools hand out, installed
// into the user skill folders of the coding agents found here. A set is
// used once accepted; its updates come at once or wait, as chosen per set.
// The user switches each agent on. The hub writes only what it installed
// and left unchanged: a skill of the user's own, or one changed by hand,
// stays and is shown as a conflict. The state arrives on `/hub/events`.
import { computed, onMounted, ref } from 'vue';
import { api, paths } from '@/api';
import type { SkillSetState, PoolSkillsState } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, useNow } from '@/lib/format';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';

const hub = useHub();
const now = useNow();
const problem = ref<string | null>(null);
const busy = ref<string | null>(null);
const state = computed<PoolSkillsState | null>(() => hub.skills);

// Read once in case the hub's stream has not told yet (an older hub never does).
onMounted(async () => {
  if (hub.skills) return;
  try {
    const r = await api().request<PoolSkillsState>('GET', paths.hubSkills);
    if (r.ok && r.body && typeof r.body === 'object') hub.skills = r.body;
    else problem.value = r.status === 404 ? 'This hub does not install skills yet: update devstack.' : `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
});

async function run(key: string, label: string, method: 'POST' | 'PUT', path: string, body: Record<string, unknown> | undefined, success: string) {
  busy.value = key;
  const r = await act<PoolSkillsState>({ label, method, path, body, success });
  busy.value = null;
  if (r.ok && r.body && typeof r.body === 'object') hub.skills = r.body;
}

const agentsOn = computed(() => (state.value?.agents ?? []).filter((a) => a.enabled).length);
const id = (s: SkillSetState) => `${s.pool}:${s.set}`;

function toggleAgent(a: { id: string; name: string; enabled: boolean }) {
  void run(
    `agent:${a.id}`,
    a.enabled ? `No skills into ${a.name}` : `Skills into ${a.name}`,
    'PUT',
    paths.skillsAgent(a.id),
    { enabled: !a.enabled },
    a.enabled ? `${a.name} gets no pool skills; what devstack installed there goes` : `${a.name} gets the accepted sets' skills`,
  );
}

function accept(s: SkillSetState, auto: boolean) {
  void run(id(s), `Accept ${s.set}`, 'POST', paths.skillSetDo(s.pool, s.set, 'accept'), { auto }, `${s.set} accepted`);
}
function decline(s: SkillSetState) {
  void run(id(s), `Decline ${s.set}`, 'POST', paths.skillSetDo(s.pool, s.set, 'decline'), undefined, `${s.set} declined`);
}
function setAuto(s: SkillSetState, auto: boolean) {
  void run(id(s), auto ? `Updates of ${s.set} at once` : `Updates of ${s.set} wait`, 'PUT', paths.skillSet(s.pool, s.set), { auto }, auto ? 'Updates come at once' : 'Updates wait for you');
}
function update(s: SkillSetState) {
  void run(id(s), `Update ${s.set}`, 'POST', paths.skillSetDo(s.pool, s.set, 'update'), undefined, `${s.set} updated`);
}
function sync() {
  void run('sync', 'Install again', 'POST', paths.skillsSync, undefined, 'The folders are as the sets say');
}

const stateText: Record<SkillSetState['state'], string> = {
  offered: 'offered',
  accepted: 'accepted',
  declined: 'declined',
  changed: 'its source changed: accept it again',
};
</script>

<template>
  <div class="page skills" data-testid="skills">
    <section class="card">
      <header class="card-header">
        <Icon name="layers" :size="18" />
        <h2>Skills</h2>
        <button type="button" class="btn btn-sm add" :disabled="busy === 'sync' || !state" title="Install what is missing again" data-testid="skills-sync" @click="sync">
          <Icon name="restart" :size="14" /> Install again
        </button>
      </header>
      <div class="card-body">
        <p class="muted">
          Skill sets your pools hand out. Once you accept a set, devstack installs its skills into the user skill folders of the coding agents you switch on below;
          an agent reads them in every repository, nothing goes into a repository. A skill of your own of the same name, or one you changed, is left as it is.
        </p>
        <p v-if="problem" class="err">{{ problem }}</p>
        <p v-if="!state && !problem" class="muted">Reading…</p>

        <template v-if="state">
          <h3>Agents</h3>
          <p v-if="!state.agents.length" class="muted small" data-testid="skills-no-agents">No coding agent found on this machine.</p>
          <p v-else-if="!agentsOn" class="warn-text small">Switch on the agents that should get the skills.</p>
          <ul class="list">
            <li v-for="a in state.agents" :key="a.id" class="item" data-testid="skills-agent" :data-id="a.id">
              <label class="toggle">
                <input type="checkbox" :checked="a.enabled" :disabled="busy === `agent:${a.id}`" @change="toggleAgent(a)" />
                <strong>{{ a.name }}</strong>
              </label>
              <code class="faint small">{{ a.dir }}</code>
            </li>
          </ul>

          <h3>Sets</h3>
          <p v-if="!state.sets.length" class="muted small" data-testid="skills-no-sets">Your pools hand out no skill sets.</p>
          <div v-for="s in state.sets" :key="id(s)" class="set" data-testid="skills-set" :data-set="id(s)">
            <div class="set-head">
              <strong>{{ s.set }}</strong>
              <span class="pill sm outline">from pool {{ s.pool }}</span>
              <span class="small" :class="{ 'warn-text': s.state === 'offered' || s.state === 'changed' }">{{ stateText[s.state] }}</span>
              <span class="faint small">{{ s.source }}<template v-if="s.commit"> · {{ s.commit }}</template><template v-if="s.fetched"> · fetched {{ ago(s.fetched * 1000, now) }}</template></span>
              <div class="actions">
                <template v-if="s.state !== 'accepted'">
                  <button type="button" class="btn btn-primary btn-sm" :disabled="busy === id(s)" data-testid="skills-accept" @click="accept(s, true)">Accept</button>
                  <button type="button" class="btn btn-sm" :disabled="busy === id(s)" title="Accept; updates wait until you take them" @click="accept(s, false)">Accept, updates wait</button>
                  <button v-if="s.state !== 'declined'" type="button" class="btn btn-ghost btn-sm" :disabled="busy === id(s)" @click="decline(s)">Decline</button>
                </template>
                <template v-else>
                  <label class="toggle small" title="Updates of this set come at once; off: they wait until you take them">
                    <input type="checkbox" :checked="s.auto" :disabled="busy === id(s)" @change="setAuto(s, !s.auto)" /> updates at once
                  </label>
                  <button v-if="s.skills.some((k) => k.pending)" type="button" class="btn btn-primary btn-sm" :disabled="busy === id(s)" data-testid="skills-update" @click="update(s)">
                    Take the updates
                  </button>
                  <button type="button" class="btn btn-ghost btn-sm" :disabled="busy === id(s)" @click="decline(s)">Remove</button>
                </template>
              </div>
            </div>
            <p v-if="s.error" class="err small">The pool's last fetch failed: {{ s.error }}</p>
            <ul class="skill-list">
              <li v-for="k in s.skills" :key="k.name" class="skill">
                <code>{{ k.name }}</code>
                <span class="faint small desc">{{ k.description }}</span>
                <span v-if="k.pending" class="pill sm warn">update waits</span>
                <span v-else-if="k.installed" class="ok-text small"><Icon name="check" :size="12" /> installed</span>
              </li>
            </ul>
          </div>

          <template v-if="state.conflicts.length || state.errors?.length">
            <h3>Left as they are</h3>
            <ul class="list">
              <li v-for="c in state.conflicts" :key="`${c.dir}/${c.name}`" class="item" data-testid="skills-conflict">
                <code>{{ c.name }}</code>
                <span class="faint small">{{ c.dir }}</span>
                <span class="small">{{ c.reason }}</span>
              </li>
              <li v-for="e in state.errors ?? []" :key="e" class="item err small">{{ e }}</li>
            </ul>
          </template>
        </template>
      </div>
    </section>
  </div>
</template>

<style scoped>
.skills {
  max-width: 900px;
}
.add {
  margin-left: auto;
}
h3 {
  margin: 20px 0 6px;
  font-size: 14px;
}
.list,
.skill-list {
  list-style: none;
  padding: 0;
  margin: 6px 0 0;
}
.item {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
  padding: 8px 0;
  border-top: 1px solid var(--border);
}
.toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.set {
  border-top: 1px solid var(--border);
  padding: 10px 0;
}
.set-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.actions {
  display: flex;
  gap: 6px;
  margin-left: auto;
  flex-wrap: wrap;
  align-items: center;
}
.skill {
  display: flex;
  align-items: baseline;
  gap: 10px;
  padding: 3px 0 3px 12px;
}
.desc {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ok-text {
  color: var(--ok-text);
}
.warn-text {
  color: var(--warn-text);
}
</style>
