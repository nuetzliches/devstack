<script setup lang="ts">
// Skills (ADR-0074, ADR-0077): the skill sets this machine's pools hand out,
// installed into the user skill folders of the coding agents switched on.
// A set is used once accepted; each of its skills can be switched off (on
// by default, a skill the set gains later too), a group at once. Its updates
// come at once or wait, as chosen per set. The hub writes only what it
// installed and left unchanged: a skill of the user's own, or one changed
// by hand, stays and is shown as a conflict. The user switches each agent
// found on. The state arrives on `/hub/events`.
import { computed, onMounted, ref } from 'vue';
import { api, paths } from '@/api';
import type { PoolSkillsState, SkillSetState, SkillState } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, useNow } from '@/lib/format';
import { counts, filtered, groupOf, groups, NO_FILTER, rows, setKey, STATUS_LABEL, type SkillFilter, type SkillRow, type SkillStatus } from '@/lib/skills';
import { useHub } from '@/stores/hub';
import Icon from '@/components/icons/Icon.vue';

const hub = useHub();
const now = useNow();
const problem = ref<string | null>(null);
const busy = ref<string | null>(null);
const state = computed<PoolSkillsState | null>(() => hub.skills);

onMounted(async () => {
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

// Sets.
const id = setKey;
function accept(s: SkillSetState, auto: boolean) {
  void run(id(s), `Accept ${s.set}`, 'POST', paths.skillSetDo(s.pool, s.set, 'accept'), { auto }, `${s.set} accepted`);
}
function decline(s: SkillSetState) {
  void run(id(s), `Decline ${s.set}`, 'POST', paths.skillSetDo(s.pool, s.set, 'decline'), undefined, `${s.set} declined`);
}
function setAuto(s: SkillSetState, auto: boolean) {
  void run(
    id(s),
    auto ? `Auto-update ${s.set}` : `No auto-update for ${s.set}`,
    'PUT',
    paths.skillSet(s.pool, s.set),
    { auto },
    auto ? `${s.set}: a new version is installed as soon as the pool has it` : `${s.set}: a new version waits until you take it`,
  );
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
const filterOn = computed(() => !!(filter.value.text || filter.value.group !== null || filter.value.status));
const statusOrder: SkillStatus[] = ['installed', 'pending', 'off', 'conflict', 'not-installed', 'not-accepted'];
const expanded = ref<Set<string>>(new Set());
function expand(key: string) {
  const s = new Set(expanded.value);
  if (s.has(key)) s.delete(key);
  else s.add(key);
  expanded.value = s;
}
/** A set's shown rows by group, in the groups' order, those without one last. */
function groupsOf(s: SkillSetState) {
  const mine = shown.value.filter((r) => r.set === s);
  const out: { group: string; rows: SkillRow[] }[] = [];
  for (const g of groupList.value) {
    const r = mine.filter((x) => groupOf(x.skill) === g);
    if (r.length) out.push({ group: g, rows: r });
  }
  return out;
}
/** Whether a set has groups at all (else its rows go without headings). */
const grouped = (s: SkillSetState) => s.skills.some((k) => k.group);
const shownIn = (s: SkillSetState) => shown.value.filter((r) => r.set === s).length;
function groupOn(s: SkillSetState, g: string) {
  return s.skills.filter((k) => groupOf(k) === g).every((k) => k.enabled !== false);
}
function switchGroup(s: SkillSetState, g: string, on: boolean) {
  const what = g || 'the skills without a group';
  void run(`group:${setKey(s)}:${g}`, on ? `Switch on ${what}` : `Switch off ${what}`, 'PUT', paths.skillGroup(s.pool, s.set, g), { enabled: on }, false);
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
    <header class="page-header">
      <div>
        <h1>Skills</h1>
        <p class="subtitle">
          Skill sets your pools hand out, installed into the user skill folders of the agents you switch on; nothing goes into a repository. A skill of your own
          of the same name, or one you changed, is left as it is. An agent sees a new or removed skill in its next session.
        </p>
      </div>
      <button type="button" class="btn btn-sm" :disabled="busy === 'sync' || !state" title="Install what is missing again" data-testid="skills-sync" @click="sync">
        <Icon name="restart" :size="14" /> Install again
      </button>
    </header>
    <p v-if="problem" class="err">{{ problem }}</p>
    <p v-if="!state && !problem" class="muted">Reading…</p>

    <template v-if="state">
      <section class="card" data-testid="skills-agents-card">
        <header class="card-header">
          <Icon name="zap" :size="16" />
          <h2>Agents</h2>
          <span class="faint small">{{ agentsOn.length }} of {{ state.agents.length }} get the skills</span>
        </header>
        <div class="card-body">
          <p v-if="!state.agents.length" class="muted small" data-testid="skills-no-agents">No coding agent found on this machine.</p>
          <p v-else-if="!agentsOn.length" class="warn-text small">Switch on the agents that should get the skills.</p>
          <div class="agents" data-testid="skills-agents">
            <label
              v-for="a in state.agents"
              :key="a.id"
              class="agent"
              :class="{ on: a.enabled }"
              :title="a.dir"
              data-testid="skills-agent"
              :data-id="a.id"
            >
              <input type="checkbox" :checked="a.enabled" :disabled="busy === `agent:${a.id}`" data-testid="skills-agent-on" @change="toggleAgent(a)" />
              <span class="agent-text">
                <strong>{{ a.name }}</strong>
                <code class="faint small">{{ a.dir }}</code>
              </span>
            </label>
          </div>
        </div>
      </section>

      <p v-if="!state.sets.length" class="muted" data-testid="skills-no-sets">Your pools hand out no skill sets.</p>

      <div v-if="all.length" class="filters" data-testid="skills-filters">
        <input v-model="filter.text" class="input search" type="search" placeholder="Search name, description, tag" spellcheck="false" data-testid="skills-search" />
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
            <Icon name="layers" :size="11" /> {{ g || 'no group' }}
          </button>
        </div>
        <button v-if="filterOn" type="button" class="btn btn-ghost btn-sm" @click="filter = { ...NO_FILTER }">Clear</button>
      </div>

      <section
        v-for="s in state.sets"
        :key="id(s)"
        class="card set"
        :class="`state-${s.state}`"
        data-testid="skills-set"
        :data-set="id(s)"
      >
        <header class="card-header set-head">
          <Icon name="box" :size="16" />
          <h2>{{ s.set }}</h2>
          <span class="pill sm outline">pool {{ s.pool }}</span>
          <span class="pill sm" :class="s.state === 'accepted' ? 'ok' : s.state === 'declined' ? '' : 'warn'">{{ stateText[s.state] }}</span>
          <div class="actions">
            <template v-if="s.state !== 'accepted'">
              <button type="button" class="btn btn-primary btn-sm" :disabled="busy === id(s)" data-testid="skills-accept" @click="accept(s, true)">Accept</button>
              <button type="button" class="btn btn-sm" :disabled="busy === id(s)" title="Accept now; a new version of a skill is installed only when you take it" @click="accept(s, false)">
                Accept without auto-update
              </button>
              <button v-if="s.state !== 'declined'" type="button" class="btn btn-ghost btn-sm" :disabled="busy === id(s)" @click="decline(s)">Decline</button>
            </template>
            <template v-else>
              <label class="toggle small" title="On: a new version of a skill is installed as soon as the pool has fetched it from the source. Off: the installed version stays until you click Take the updates.">
                <input type="checkbox" :checked="s.auto" :disabled="busy === id(s)" @change="setAuto(s, !s.auto)" /> auto-update
              </label>
              <button v-if="waiting(s)" type="button" class="btn btn-primary btn-sm" :disabled="busy === id(s)" data-testid="skills-update" @click="update(s)">
                Take the updates
              </button>
              <button type="button" class="btn btn-ghost btn-sm" :disabled="busy === id(s)" @click="decline(s)">Remove</button>
            </template>
          </div>
        </header>
        <div class="card-body">
          <p class="source faint small">
            <code>{{ s.source }}</code><template v-if="s.commit"> · {{ s.commit }}</template
            ><template v-if="s.fetched"> · fetched by the pool {{ ago(s.fetched * 1000, now) }}</template> · {{ s.skills.length }} skill(s)
          </p>
          <p v-if="s.error" class="err small">The pool's last fetch failed: {{ s.error }}</p>
          <p v-if="s.state !== 'accepted'" class="muted small">
            {{ s.state === 'declined' ? 'Declined: none of its skills is installed.' : 'Accept the set to install its skills; you can switch single ones off then.' }}
          </p>
          <p v-if="filterOn && !shownIn(s)" class="faint small">No skill of this set matches the filter.</p>

          <div v-for="g in groupsOf(s)" :key="g.group || '-'" class="group" data-testid="skills-group" :data-group="g.group">
            <div v-if="grouped(s)" class="group-head">
              <span class="group-name">{{ g.group || 'No group' }}</span>
              <span class="faint small">{{ g.rows.length }}</span>
              <label v-if="s.state === 'accepted'" class="toggle small" :title="`Switch every skill of ${g.group || 'no group'} on or off`">
                <input
                  type="checkbox"
                  :checked="groupOn(s, g.group)"
                  :disabled="busy === `group:${id(s)}:${g.group}`"
                  data-testid="skills-group-on"
                  @change="switchGroup(s, g.group, !groupOn(s, g.group))"
                />
                all on
              </label>
            </div>
            <ul class="skill-list">
              <li v-for="r in g.rows" :key="r.key" class="skill" :class="{ off: r.status === 'off' || r.status === 'not-accepted' }" data-testid="skills-skill" :data-name="r.skill.name">
                <div class="skill-row">
                  <input
                    v-if="s.state === 'accepted'"
                    type="checkbox"
                    :checked="r.skill.enabled !== false"
                    :disabled="busy === r.key"
                    :title="r.skill.enabled !== false ? 'Switch off: removed from the agents\' folders' : 'Switch on'"
                    data-testid="skills-skill-on"
                    @change="switchSkill(r, r.skill.enabled === false)"
                  />
                  <span v-else class="no-switch" aria-hidden="true" />
                  <button type="button" class="name" :aria-expanded="expanded.has(r.key)" @click="expand(r.key)">
                    <Icon :name="expanded.has(r.key) ? 'chevronDown' : 'chevronRight'" :size="12" class="faint" />
                    <code>{{ r.skill.name }}</code>
                  </button>
                  <span class="desc faint small" :title="r.skill.description">{{ r.skill.description }}</span>
                  <span v-for="t in r.skill.tags ?? []" :key="t" class="pill sm outline tag">{{ t }}</span>
                  <span v-if="s.state === 'accepted'" class="status small" :class="statusClass[r.status]">
                    <Icon v-if="r.status === 'installed'" name="check" :size="12" /> {{ STATUS_LABEL[r.status] }}
                  </span>
                  <span v-if="s.state === 'accepted'" class="faint small updated" :title="r.skill.updated ? new Date(r.skill.updated).toLocaleString() : 'not written here yet'">
                    {{ r.skill.updated ? ago(r.skill.updated, now) : '' }}
                  </span>
                </div>
                <div v-if="expanded.has(r.key)" class="detail small" data-testid="skills-skill-detail">
                  <p>{{ r.skill.description }}</p>
                  <dl>
                    <dt>Version</dt>
                    <dd><code>{{ r.skill.sha256.slice(0, 12) }}</code><template v-if="s.commit"> · commit {{ s.commit }}</template></dd>
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
        </div>
      </section>

      <section v-if="state.conflicts.length || state.errors?.length" class="card" data-testid="skills-left">
        <header class="card-header">
          <Icon name="alert" :size="16" />
          <h2>Left as they are</h2>
        </header>
        <div class="card-body">
          <ul class="list">
            <li v-for="c in state.conflicts" :key="`${c.dir}/${c.name}`" class="item" data-testid="skills-conflict">
              <code>{{ c.name }}</code>
              <span class="faint small">{{ c.dir }}</span>
              <span class="small">{{ c.reason }}</span>
            </li>
            <li v-for="e in state.errors ?? []" :key="e" class="item err small">{{ e }}</li>
          </ul>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
.skills {
  max-width: 1000px;
}
.subtitle {
  max-width: 720px;
}
.agents {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
  gap: 8px;
}
.agent {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 8px;
  cursor: pointer;
}
.agent input {
  margin-top: 3px;
}
.agent.on {
  border-color: var(--ok-text);
}
.agent-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.agent-text code {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  position: sticky;
  top: 0;
  z-index: 2;
  padding: 8px 0;
  background: var(--bg);
}
.search {
  flex: 1;
  min-width: 220px;
}
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 9px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
}
.chip.active {
  border-color: var(--border-strong);
  background: var(--surface-hover);
}
.set-head {
  flex-wrap: wrap;
  gap: 8px;
}
.set.state-offered,
.set.state-changed {
  border-color: var(--warn-text);
}
.actions {
  display: flex;
  gap: 6px;
  margin-left: auto;
  flex-wrap: wrap;
  align-items: center;
}
.toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.source {
  margin: 0 0 8px;
}
.group + .group {
  margin-top: 14px;
}
.group-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-radius: 6px;
  background: var(--surface-hover);
}
.group-name {
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}
.group-head .toggle {
  margin-left: auto;
}
.skill-list,
.list {
  list-style: none;
  padding: 0;
  margin: 4px 0 0;
}
.skill + .skill {
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
  padding: 6px 8px;
}
.no-switch {
  display: inline-block;
  width: 13px;
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
  padding: 2px 8px 10px 56px;
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
.item {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
  padding: 8px 0;
  border-top: 1px solid var(--border);
}
.ok-text {
  color: var(--ok-text);
}
.warn-text {
  color: var(--warn-text);
}
</style>
