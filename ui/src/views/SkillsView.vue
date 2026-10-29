<script setup lang="ts">
// Skills (ADR-0074, ADR-0077): the skill sets this machine's pools hand out,
// installed into the user skill folders of the coding agents switched on.
// A set is used once accepted; each of its skills can be switched off (on
// by default, a skill the set gains later too), a group at once. Its updates
// come at once or wait, as chosen per set. The hub writes only what it
// installed and left unchanged: a skill of the user's own, or one changed
// by hand, stays and is shown as a conflict. Which agents get the skills is
// chosen in "Programs and agents". The state arrives on `/hub/events`.
import { computed, onMounted, ref } from 'vue';
import { api, paths } from '@/api';
import type { PoolSkillsState, SkillSetState, SkillState } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, useNow } from '@/lib/format';
import { counts, filtered, groupOf, groups, NO_FILTER, rows, setKey, STATUS_LABEL, type SkillFilter, type SkillRow, type SkillStatus } from '@/lib/skills';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';
import OpenersDialog from '@/components/OpenersDialog.vue';

const hub = useHub();
const now = useNow();
const problem = ref<string | null>(null);
const busy = ref<string | null>(null);
const state = computed<PoolSkillsState | null>(() => hub.skills);
const agentsDialog = ref(false);

onMounted(async () => {
  void hub.loadOpeners();
  if (hub.skills) return;
  // Once, in case the hub's stream has not told yet (an older hub never does).
  try {
    const r = await api().request<PoolSkillsState>('GET', paths.hubSkills);
    if (r.ok && r.body && typeof r.body === 'object') hub.skills = r.body;
    else problem.value = r.status === 404 ? 'This hub does not install skills yet: update devstack.' : `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
});

async function run(key: string, label: string, method: 'POST' | 'PUT', path: string, body: Record<string, unknown> | undefined, success: string | false) {
  busy.value = key;
  const r = await act<PoolSkillsState>({ label, method, path, body, success });
  busy.value = null;
  if (r.ok && r.body && typeof r.body === 'object') hub.skills = r.body;
}

const agentsOn = computed(() => (state.value?.agents ?? []).filter((a) => a.enabled));

// Sets.
const id = setKey;
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
const waiting = (s: SkillSetState) => s.skills.some((k) => k.pending && k.enabled !== false);

// Skills.
const all = computed<SkillRow[]>(() => rows(state.value));
const filter = ref<SkillFilter>({ ...NO_FILTER });
const shown = computed(() => filtered(all.value, filter.value));
const statusCounts = computed(() => counts(all.value));
const groupList = computed(() => groups(all.value));
const filterOn = computed(() => !!(filter.value.text || filter.value.set || filter.value.group !== null || filter.value.status));
const statusOrder: SkillStatus[] = ['installed', 'pending', 'off', 'conflict', 'not-installed', 'not-accepted'];
const expanded = ref<Set<string>>(new Set());
function expand(key: string) {
  const s = new Set(expanded.value);
  if (s.has(key)) s.delete(key);
  else s.add(key);
  expanded.value = s;
}
/** The shown rows by group, groups in their order, those without one last. */
const byGroup = computed(() => {
  const out: { group: string; rows: SkillRow[] }[] = [];
  for (const g of groupList.value) {
    const r = shown.value.filter((x) => groupOf(x.skill) === g);
    if (r.length) out.push({ group: g, rows: r });
  }
  return out;
});
/** One set's accepted rows of a group, to switch them together. */
function groupSets(g: string) {
  const sets = new Map<string, SkillSetState>();
  for (const r of all.value) if (groupOf(r.skill) === g && r.set.state === 'accepted') sets.set(setKey(r.set), r.set);
  return [...sets.values()];
}
function groupOn(g: string) {
  return all.value.filter((r) => groupOf(r.skill) === g && r.set.state === 'accepted').every((r) => r.skill.enabled !== false);
}
async function switchGroup(g: string, on: boolean) {
  for (const s of groupSets(g)) {
    await run(`group:${g}`, on ? `Switch on ${g || 'the skills without a group'}` : `Switch off ${g || 'the skills without a group'}`, 'PUT', paths.skillGroup(s.pool, s.set, g), { enabled: on }, false);
  }
}
function switchSkill(r: SkillRow, on: boolean) {
  void run(
    r.key,
    on ? `Switch on ${r.skill.name}` : `Switch off ${r.skill.name}`,
    'PUT',
    paths.skillSwitch(r.set.pool, r.set.set, r.skill.name),
    { enabled: on },
    on ? `${r.skill.name} is installed again` : `${r.skill.name} is off: removed from the agents' folders`,
  );
}
const statusClass: Record<SkillStatus, string> = {
  installed: 'ok-text',
  pending: 'warn-text',
  off: 'faint',
  conflict: 'warn-text',
  'not-installed': 'faint',
  'not-accepted': 'faint',
};
const conflictOf = (k: SkillState) => state.value?.conflicts.find((c) => c.name === k.name);
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
          Skill sets your pools hand out. Once you accept a set, devstack installs its skills into the user skill folders of the coding agents you chose; an agent
          reads them in every repository, nothing goes into a repository. A skill of your own of the same name, or one you changed, is left as it is. An agent
          sees a new or removed skill in its next session.
        </p>
        <p v-if="problem" class="err">{{ problem }}</p>
        <p v-if="!state && !problem" class="muted">Reading…</p>

        <template v-if="state">
          <div class="agents" data-testid="skills-agents">
            <span class="label">Into</span>
            <span v-for="a in agentsOn" :key="a.id" class="pill sm outline" :title="a.dir">{{ a.name }}</span>
            <span v-if="!agentsOn.length" class="warn-text small">no agent yet</span>
            <button type="button" class="btn btn-ghost btn-sm" data-testid="skills-agents-manage" @click="agentsDialog = true">
              <Icon name="wrench" :size="13" /> Programs and agents…
            </button>
          </div>

          <h3>Sets</h3>
          <p v-if="!state.sets.length" class="muted small" data-testid="skills-no-sets">Your pools hand out no skill sets.</p>
          <div v-for="s in state.sets" :key="id(s)" class="set" data-testid="skills-set" :data-set="id(s)">
            <div class="set-head">
              <strong>{{ s.set }}</strong>
              <span class="pill sm outline">from pool {{ s.pool }}</span>
              <span class="small" :class="{ 'warn-text': s.state === 'offered' || s.state === 'changed' }">{{ stateText[s.state] }}</span>
              <span class="faint small"
                >{{ s.source }}<template v-if="s.commit"> · {{ s.commit }}</template
                ><template v-if="s.fetched"> · fetched by the pool {{ ago(s.fetched * 1000, now) }}</template> · {{ s.skills.length }} skill(s)</span
              >
              <div class="actions">
                <template v-if="s.state !== 'accepted'">
                  <button type="button" class="btn btn-primary btn-sm" :disabled="busy === id(s)" data-testid="skills-accept" @click="accept(s, true)">Accept</button>
                  <button type="button" class="btn btn-sm" :disabled="busy === id(s)" title="Accept; updates wait until you take them" @click="accept(s, false)">
                    Accept, updates wait
                  </button>
                  <button v-if="s.state !== 'declined'" type="button" class="btn btn-ghost btn-sm" :disabled="busy === id(s)" @click="decline(s)">Decline</button>
                </template>
                <template v-else>
                  <label class="toggle small" title="Updates of this set come at once; off: they wait until you take them">
                    <input type="checkbox" :checked="s.auto" :disabled="busy === id(s)" @change="setAuto(s, !s.auto)" /> updates at once
                  </label>
                  <button v-if="waiting(s)" type="button" class="btn btn-primary btn-sm" :disabled="busy === id(s)" data-testid="skills-update" @click="update(s)">
                    Take the updates
                  </button>
                  <button type="button" class="btn btn-ghost btn-sm" :disabled="busy === id(s)" @click="decline(s)">Remove</button>
                </template>
              </div>
            </div>
            <p v-if="s.error" class="err small">The pool's last fetch failed: {{ s.error }}</p>
          </div>

          <template v-if="all.length">
            <h3>Skills</h3>
            <div class="filters" data-testid="skills-filters">
              <input v-model="filter.text" class="input search" type="search" placeholder="Search name, description, tag" spellcheck="false" data-testid="skills-search" />
              <select v-if="state.sets.length > 1" v-model="filter.set" class="input" data-testid="skills-filter-set">
                <option value="">every set</option>
                <option v-for="s in state.sets" :key="id(s)" :value="id(s)">{{ s.set }} ({{ s.pool }})</option>
              </select>
              <div class="chips">
                <button
                  v-for="st in statusOrder.filter((x) => statusCounts[x])"
                  :key="st"
                  type="button"
                  class="chip"
                  :class="{ active: filter.status === st }"
                  :data-testid="`skills-status-${st}`"
                  @click="filter.status = filter.status === st ? '' : st"
                >
                  {{ STATUS_LABEL[st] }} <span class="faint">{{ statusCounts[st] }}</span>
                </button>
              </div>
              <div v-if="groupList.length > 1" class="chips">
                <button
                  v-for="g in groupList"
                  :key="g || '-'"
                  type="button"
                  class="chip"
                  :class="{ active: filter.group === g }"
                  data-testid="skills-group-chip"
                  @click="filter.group = filter.group === g ? null : g"
                >
                  {{ g || 'no group' }}
                </button>
              </div>
              <button v-if="filterOn" type="button" class="btn btn-ghost btn-sm" @click="filter = { ...NO_FILTER }">Clear</button>
            </div>

            <p v-if="!shown.length" class="muted small">No skill matches.</p>
            <div v-for="g in byGroup" :key="g.group || '-'" class="group" data-testid="skills-group" :data-group="g.group">
              <div v-if="groupList.length > 1" class="group-head">
                <strong class="small">{{ g.group || 'No group' }}</strong>
                <span class="faint small">{{ g.rows.length }}</span>
                <label v-if="groupSets(g.group).length" class="toggle small" :title="`Switch every skill of ${g.group || 'no group'} on or off`">
                  <input type="checkbox" :checked="groupOn(g.group)" :disabled="busy === `group:${g.group}`" data-testid="skills-group-on" @change="switchGroup(g.group, !groupOn(g.group))" />
                  all on
                </label>
              </div>
              <ul class="skill-list">
                <li v-for="r in g.rows" :key="r.key" class="skill" :class="{ off: r.status === 'off' }" data-testid="skills-skill" :data-name="r.skill.name">
                  <div class="skill-row">
                    <input
                      type="checkbox"
                      :checked="r.skill.enabled !== false"
                      :disabled="r.set.state !== 'accepted' || busy === r.key"
                      :title="r.set.state !== 'accepted' ? 'Accept its set first' : r.skill.enabled !== false ? 'Switch off: removed from the agents\' folders' : 'Switch on'"
                      data-testid="skills-skill-on"
                      @change="switchSkill(r, r.skill.enabled === false)"
                    />
                    <button type="button" class="name" :aria-expanded="expanded.has(r.key)" @click="expand(r.key)">
                      <Icon :name="expanded.has(r.key) ? 'chevronDown' : 'chevronRight'" :size="12" class="faint" />
                      <code>{{ r.skill.name }}</code>
                    </button>
                    <span class="desc faint small" :title="r.skill.description">{{ r.skill.description }}</span>
                    <span v-for="t in r.skill.tags ?? []" :key="t" class="pill sm outline tag">{{ t }}</span>
                    <span v-if="state.sets.length > 1" class="faint small">{{ r.set.set }}</span>
                    <span class="status small" :class="statusClass[r.status]">
                      <Icon v-if="r.status === 'installed'" name="check" :size="12" /> {{ STATUS_LABEL[r.status] }}
                    </span>
                    <span class="faint small updated" :title="r.skill.updated ? new Date(r.skill.updated).toLocaleString() : 'not written here yet'">
                      {{ r.skill.updated ? ago(r.skill.updated, now) : '' }}
                    </span>
                  </div>
                  <div v-if="expanded.has(r.key)" class="detail small" data-testid="skills-skill-detail">
                    <p>{{ r.skill.description }}</p>
                    <dl>
                      <dt>Set</dt>
                      <dd>{{ r.set.set }} from pool {{ r.set.pool }} · {{ r.set.source }}<template v-if="r.set.commit"> · {{ r.set.commit }}</template></dd>
                      <dt>Version</dt>
                      <dd><code>{{ r.skill.sha256.slice(0, 12) }}</code></dd>
                      <dt>Updated here</dt>
                      <dd>{{ r.skill.updated ? new Date(r.skill.updated).toLocaleString() : 'not yet' }}</dd>
                      <dt>In</dt>
                      <dd>{{ r.skill.agents?.length ? r.skill.agents.join(', ') : 'no agent' }}</dd>
                      <template v-if="conflictOf(r.skill)">
                        <dt>Conflict</dt>
                        <dd class="warn-text">{{ conflictOf(r.skill)?.reason }} ({{ conflictOf(r.skill)?.dir }})</dd>
                      </template>
                    </dl>
                  </div>
                </li>
              </ul>
            </div>
          </template>

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
    <OpenersDialog :open="agentsDialog" :openers="hub.openers ?? []" @close="agentsDialog = false" />
  </div>
</template>

<style scoped>
.skills {
  max-width: 1000px;
}
.add {
  margin-left: auto;
}
h3 {
  margin: 20px 0 6px;
  font-size: 14px;
}
.agents {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: 12px;
}
.agents .label {
  font-size: 13px;
  color: var(--text-muted);
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
.filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}
.search {
  flex: 1;
  min-width: 200px;
}
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}
.chip {
  padding: 2px 9px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
}
.chip.active {
  border-color: var(--accent, var(--border-strong));
  background: var(--surface-hover);
}
.group {
  margin-top: 10px;
}
.group-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
}
.group-head .toggle {
  margin-left: auto;
}
.skill {
  border-top: 1px solid var(--border);
}
.skill.off .name,
.skill.off .desc {
  opacity: 0.55;
}
.skill-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 4px;
}
.name {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 0;
  border: 0;
  background: transparent;
  color: inherit;
  cursor: pointer;
}
.desc {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tag {
  flex: none;
}
.status {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  flex: none;
  min-width: 96px;
  justify-content: flex-end;
}
.updated {
  flex: none;
  min-width: 72px;
  text-align: right;
}
.detail {
  padding: 2px 8px 10px 52px;
}
.detail p {
  margin: 0 0 6px;
}
.detail dl {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 2px 12px;
  margin: 0;
}
.detail dt {
  color: var(--text-muted);
}
.detail dd {
  margin: 0;
}
.ok-text {
  color: var(--ok-text);
}
.warn-text {
  color: var(--warn-text);
}
</style>
