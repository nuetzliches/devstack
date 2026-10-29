<script setup lang="ts">
// Programs and agents (ADR-0046, ADR-0077): the path menu's programs, their
// order and which it lists (in this browser), and for each coding agent
// whether the pools' skills go into its user skill folder (on this machine,
// kept by the hub). An agent the menu cannot open (a CLI such as Codex) is
// listed for its skills alone. A change applies at once.
import { computed } from 'vue';
import { paths } from '@/api';
import type { Opener, PoolSkillsState } from '@/api/types';
import { act } from '@/lib/actions';
import { arrange, useOpenerPrefs } from '@/lib/openers';
import { useHub } from '@/stores/hub';
import Icon from './icons/Icon.vue';
import Modal from './Modal.vue';

const props = defineProps<{ open: boolean; openers: Opener[] }>();
defineEmits<{ close: [] }>();

const hub = useHub();
const { prefs, save } = useOpenerPrefs();
const list = computed(() => arrange(props.openers, prefs.value));
const kindLabel = { files: 'files', terminal: 'terminal', editor: 'editor', git: 'Git client', agent: 'agent' } as const;
const kindIcon = { files: 'folder', terminal: 'terminal', editor: 'file', git: 'gitBranch', agent: 'zap' } as const;

/** The agents with a skill folder, by id; none from a hub before skill sets. */
const skillAgents = computed(() => new Map((hub.skills?.agents ?? []).map((a) => [a.id, a])));
/** Agents with a skill folder that no program of the menu stands for. */
const onlySkills = computed(() => {
  const inMenu = new Set(props.openers.map((o) => o.agent).filter(Boolean));
  return (hub.skills?.agents ?? []).filter((a) => !inMenu.has(a.id));
});

function move(from: number, to: number) {
  if (to < 0 || to >= list.value.length || from === to) return;
  const ids = list.value.map((o) => o.id);
  const [id] = ids.splice(from, 1);
  ids.splice(to, 0, id);
  save({ ...prefs.value, order: ids });
}
function toggle(o: Opener) {
  const hidden = prefs.value.hidden.includes(o.id) ? prefs.value.hidden.filter((h) => h !== o.id) : [...prefs.value.hidden, o.id];
  save({ ...prefs.value, hidden });
}
function reset() {
  save({ order: [], hidden: [] });
}

async function setSkills(id: string, enabled: boolean) {
  const a = skillAgents.value.get(id);
  if (!a) return;
  const r = await act<PoolSkillsState>({
    label: enabled ? `Skills into ${a.name}` : `No skills into ${a.name}`,
    method: 'PUT',
    path: paths.skillsAgent(id),
    body: { enabled },
    success: enabled ? `${a.name} gets the accepted sets' skills` : `${a.name} gets no pool skills; what devstack installed there goes`,
  });
  if (r.ok && r.body && typeof r.body === 'object') hub.skills = r.body;
}
</script>

<template>
  <Modal :open="open" title="Programs and agents" size="sm" testid="openers-dialog" @close="$emit('close')">
    <h3 class="section">Path menu</h3>
    <p class="muted small intro">Move them with the arrows; the ones unchecked are not listed. Kept in this browser.</p>
    <ul class="list">
      <li
        v-for="(o, i) in list"
        :key="o.id"
        class="row"
        :class="{ off: prefs.hidden.includes(o.id) }"
        data-testid="opener-row"
        :data-id="o.id"
      >
        <label class="grow pick">
          <input type="checkbox" :checked="!prefs.hidden.includes(o.id)" data-testid="opener-shown" @change="toggle(o)" />
          <Icon :name="kindIcon[o.kind] ?? 'external'" :size="14" class="muted" />
          <span class="grow truncate">{{ o.label }}</span>
          <span class="faint small">{{ kindLabel[o.kind] ?? o.kind }}</span>
        </label>
        <label
          v-if="o.agent && skillAgents.has(o.agent)"
          class="skills-pick"
          :class="{ on: skillAgents.get(o.agent)?.enabled }"
          :title="`The pools' skills into ${skillAgents.get(o.agent)?.dir}; kept on this machine`"
        >
          <input
            type="checkbox"
            :checked="skillAgents.get(o.agent)?.enabled"
            data-testid="opener-skills"
            @change="setSkills(o.agent, !skillAgents.get(o.agent)?.enabled)"
          />
          skills
        </label>
        <button type="button" class="move" :disabled="i === 0" :aria-label="`Move ${o.label} up`" data-testid="opener-up" @click="move(i, i - 1)">
          <Icon name="arrowUp" :size="13" />
        </button>
        <button type="button" class="move" :disabled="i === list.length - 1" :aria-label="`Move ${o.label} down`" data-testid="opener-down" @click="move(i, i + 1)">
          <Icon name="arrowDown" :size="13" />
        </button>
      </li>
    </ul>

    <template v-if="hub.skills && onlySkills.length">
      <h3 class="section">Agents for skills</h3>
      <p class="muted small intro">
        Coding agents found here that the menu does not open. Checked ones get the skills of the sets you accepted (<router-link to="/skills" @click="$emit('close')"
          >Skills</router-link
        >). Kept on this machine.
      </p>
      <ul class="list">
        <li v-for="a in onlySkills" :key="a.id" class="row" data-testid="skill-agent-row" :data-id="a.id">
          <label class="grow pick">
            <input type="checkbox" :checked="a.enabled" data-testid="skill-agent-on" @change="setSkills(a.id, !a.enabled)" />
            <Icon name="zap" :size="14" class="muted" />
            <span class="grow truncate">{{ a.name }}</span>
            <code class="faint small truncate dir" :title="a.dir">{{ a.dir }}</code>
          </label>
        </li>
      </ul>
    </template>
    <template #footer>
      <button type="button" class="btn btn-sm" :disabled="!prefs.order.length && !prefs.hidden.length" data-testid="openers-reset" title="The menu's order and choice as found" @click="reset">
        Reset menu
      </button>
      <button type="button" class="btn btn-sm btn-primary" @click="$emit('close')">Done</button>
    </template>
  </Modal>
</template>

<style scoped>
.section {
  margin: 0 0 4px;
  font-size: 13px;
}
.section:not(:first-child) {
  margin-top: 16px;
}
.intro {
  margin: 0 0 10px;
}
.list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 2px 8px;
  border-radius: 8px;
}
/* Ghost buttons, as the branch picker's "+". */
.move {
  display: inline-flex;
  padding: 4px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
}
.move:hover:not(:disabled) {
  border-color: var(--border-strong);
  color: var(--text);
}
.move:disabled {
  opacity: 0.35;
  cursor: default;
}
.skills-pick {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 1px 8px 1px 4px;
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--text-muted);
  font-size: 11px;
  cursor: pointer;
}
.skills-pick.on {
  border-color: var(--ok-text);
  color: var(--ok-text);
}
.row:hover {
  background: var(--surface-hover);
}
.row.off .pick > :not(input) {
  opacity: 0.5;
}
.pick {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 4px;
  font-size: 13px;
  cursor: pointer;
  min-width: 0;
}
.grow {
  flex: 1;
  min-width: 0;
}
.dir {
  max-width: 45%;
}
.small {
  font-size: 12px;
}
</style>
