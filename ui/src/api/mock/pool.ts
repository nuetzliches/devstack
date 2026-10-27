// A fake pool for the mock hub (ADR-0031, ADR-0032): this machine is an admin,
// two colleagues are members, a request waits. `?mock=nopool` is a machine in
// no pool, `?mock=poolmember` one without the admin role. A provisioner made
// two nodes (ADR-0051): one runs a lane, one waits for an admin to confirm it;
// a third machine's enrolment token was used twice (an alarm).
import type { PoolAuditEntry, PoolLease, PoolMember, PoolNode, PoolOverview, PoolRole, Provisioning, ShareStatus } from '../types';

type R = { status: number; body: unknown };

export class MockPool {
  joined: boolean;
  myRole: PoolRole;
  members: PoolMember[];
  nodes: PoolNode[];
  audit: PoolAuditEntry[] = [];
  leases: PoolLease[] = [];
  provisioning: Provisioning;
  /** This machine keeps its lanes off provisioned nodes. */
  noProvisioned = false;
  private me = 'a1b2c3d4e5f60718';
  /** `GET /hub/pool/share`: a machine with 16 cores and 64 GiB, about a third busy. */
  share(): ShareStatus {
    const free = { cpus: 10.4 + Math.random(), memory: 38 * 2 ** 30 };
    return {
      total: { cpus: 16, memory: 64 * 2 ** 30 },
      free,
      member: this.joined,
      reported_at: this.joined ? Date.now() - 4000 : null,
      error: null,
      unavailable: "A machine runs other members' lanes with a node agent (`devstack node agent`, granted by an admin); the dashboard does not start one yet (#171, on Windows #168).",
    };
  }

  constructor(q: string) {
    const s = Math.floor(Date.now() / 1000);
    this.joined = !q.includes('nopool');
    this.myRole = q.includes('poolmember') ? 'member' : 'admin';
    this.members = [
      { id: this.me, name: 'dev-pc', state: 'approved', role: this.myRole, requested_at: s - 86_400 * 3, address: '10.0.0.21', tags: ['team:backend'] },
      { id: '9f8e7d6c5b4a3921', name: 'build-server', state: 'approved', role: this.myRole === 'admin' ? 'member' : 'admin', requested_at: s - 86_400 * 5, address: '10.0.0.14', node: true, policy: { allow: ['team:backend'], repos: [], memory_headroom: 2, network: 'none' } },
      { id: '1122334455667788', name: 'laptop-anna', state: 'approved', role: 'member', requested_at: s - 86_400, address: '100.64.0.9' },
      { id: '5566778899aabbcc', name: 'laptop-ben', state: 'pending', role: 'member', requested_at: s - 240, address: '100.64.0.12' },
      { id: 'c0ffee0011223344', name: 'cloud-1', state: 'approved', role: 'member', requested_at: s - 3600, address: '203.0.113.7', node: true, provisioned: { request: 'r-7f3a', machine: 'srv-4815', address: '203.0.113.7', confirmed: true } },
      { id: 'c0ffee5566778899', name: 'cloud-2', state: 'approved', role: 'member', requested_at: s - 120, address: '203.0.113.9', node: true, provisioned: { request: 'r-8b1c', machine: 'srv-4816', address: '203.0.113.9' } },
    ];
    this.members[0].provision = true;
    this.nodes = [
      { id: this.me, name: 'dev-pc', online: true, seen: 4, report: { sharing: false, cpus: 0, memory: 0 }, kind: 'machine' },
      { id: '9f8e7d6c5b4a3921', name: 'build-server', online: true, seen: 7, report: { sharing: true, cpus: 12, memory: 48 * 2 ** 30, api: 'https://10.0.0.14:4261', devstack: { version: '0.10.0', protocol: 1 }, total: { cpus: 32, memory: 128 * 2 ** 30 }, used: { cpus: 6.5, memory: 22 * 2 ** 30 }, kept: { lanes: 1, bytes: 5 * 2 ** 30, store_bytes: 2 * 2 ** 30 }, gpus: [{ name: 'NVIDIA L4', memory: 24 * 2 ** 30, memory_used: 3 * 2 ** 30, load: 18, offered: true }] }, kind: 'server' },
      { id: '1122334455667788', name: 'laptop-anna', online: false, seen: 7200, kind: 'machine' },
      {
        id: 'c0ffee0011223344', name: 'cloud-1', online: true, seen: 3, kind: 'server',
        report: { sharing: true, cpus: 7.2, memory: 28 * 2 ** 30, api: 'https://203.0.113.7:4261', devstack: { version: '0.10.0', protocol: 1 }, total: { cpus: 8, memory: 32 * 2 ** 30 }, used: { cpus: 1.5, memory: 6 * 2 ** 30 } },
        provisioned: { machine: 'srv-4815', address: '203.0.113.7', key: '4e1f…9a07', size: { cpus: 8, memory: 32 * 2 ** 30 }, since: s - 3500 },
      },
      {
        id: 'c0ffee5566778899', name: 'cloud-2', online: true, seen: 5, kind: 'server',
        report: { sharing: true, cpus: 7.6, memory: 30 * 2 ** 30, api: 'https://203.0.113.9:4261', devstack: { version: '0.10.0', protocol: 1 }, total: { cpus: 8, memory: 32 * 2 ** 30 }, used: { cpus: 0, memory: 0 } },
        provisioned: { machine: 'srv-4816', address: '203.0.113.9', key: 'b7d2c19e5a0f3e88d41c6b2a97f05e3d1c8a4b6f20e9d7c5a3b1f0e2d4c6a8b9', size: { cpus: 8, memory: 32 * 2 ** 30 }, since: s - 110, idle_left: 1690, unconfirmed: true },
      },
    ];
    const machine = { size: { cpus: 8, memory: 32 * 2 ** 30 }, requester: this.me, deadline: s + 600 };
    this.provisioning = {
      provisioner: 'https://provisioner.example:8443',
      settings: { per: 'pool', confirm: 'admin', idle_minutes: 30, max_nodes: 3 },
      machines: [
        { ...machine, request: 'r-7f3a', machine: 'srv-4815', address: '203.0.113.7', state: 'ready', created: s - 3600, member: 'c0ffee0011223344' },
        { ...machine, request: 'r-8b1c', machine: 'srv-4816', address: '203.0.113.9', state: 'ready', created: s - 180, member: 'c0ffee5566778899', idle_since: s - 110 },
        { ...machine, request: 'r-9d2e', machine: 'srv-4817', address: '203.0.113.11', state: 'provisioning', created: s - 90, requester: '1122334455667788', alarm: 'its enrolment token was used a second time, from 198.51.100.23' },
      ],
    };
    this.leases = [
      { id: '7c1e9a0b22d4f610', node: '9f8e7d6c5b4a3921', member: this.me, repo: 'shop', lane: 'review-cart', state: 'running', created: s - 1800, expires_in: 110 },
      { id: '0d3a5b6c7e8f9012', node: '9f8e7d6c5b4a3921', member: '1122334455667788', repo: 'crm', lane: 'agent-1242', state: 'running', created: s - 600, expires_in: 95 },
      { id: '3e4f5a6b7c8d9e0f', node: 'c0ffee0011223344', member: this.me, repo: 'shop', lane: 'load-test', state: 'running', created: s - 900, expires_in: 100 },
    ];
    this.audit = [{ at: s - 86_400, by: `${this.me} (dev-pc)`, action: 'approve', member: '1122334455667788', name: 'laptop-anna', detail: 'member' }];
  }

  private visible(m: PoolMember): PoolMember {
    if (this.myRole === 'admin') return m;
    const { address: _hidden, ...rest } = m;
    return rest;
  }

  overview(): PoolOverview {
    if (!this.joined) return { joined: false };
    const me = { id: this.me, name: 'dev-pc', state: 'approved' as const, role: this.myRole, coordinator: 'https://10.0.0.14:4260', no_provisioned: this.noProvisioned };
    const v: PoolOverview = {
      joined: true,
      me,
      nodes: this.nodes.filter((n) => this.members.some((m) => m.id === n.id && m.state === 'approved')),
      members: this.members.filter((m) => this.myRole === 'admin' || m.state === 'approved').map((m) => this.visible(m)),
      leases: this.leases,
      join: {
        coordinator: me.coordinator,
        fingerprint: '1a93c4d2e5f60718293a4b5c6d7e8f901a2b3c4d5e6f708192a3b4c5d6e7f809',
        command: `devstack join ${me.coordinator} --fingerprint 1a93c4d2e5f60718293a4b5c6d7e8f901a2b3c4d5e6f708192a3b4c5d6e7f809`,
      },
    };
    if (this.myRole === 'admin') {
      v.audit = this.audit;
      v.provisioning = this.provisioning;
    }
    return v;
  }

  private log(action: PoolAuditEntry['action'], m: PoolMember, detail?: string) {
    this.audit.unshift({ at: Math.floor(Date.now() / 1000), by: `${this.me} (dev-pc)`, action, member: m.id, name: m.name, detail });
  }

  private lastAdmin(m: PoolMember) {
    return m.role === 'admin' && m.state === 'approved' && this.members.filter((x) => x.role === 'admin' && x.state === 'approved').length === 1;
  }

  /** `POST /hub/pool/nodes/{id}/pause|resume`. */
  pause(id: string, paused: boolean): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const n = this.nodes.find((x) => x.id === id);
    const m = this.members.find((x) => x.id === id);
    if (!n || !m) return { status: 404, body: { error: `no member \`${id}\`` } };
    n.paused = paused;
    this.log(paused ? 'pause' : 'resume', m);
    return { status: 200, body: this.visible(m) };
  }

  /** `POST /hub/pool/leases/{id}/end`. */
  end(id: string): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const l = this.leases.find((x) => x.id === id);
    const m = l && this.members.find((x) => x.id === l.node);
    if (!l || !m) return { status: 404, body: { error: `no lease \`${id}\`` } };
    this.leases = this.leases.filter((x) => x !== l);
    this.log('end', m, `${l.repo}/${l.lane} (${l.id})`);
    return { status: 202, body: null };
  }

  /** `POST /hub/pool/members/{id}/tags` and `.../nodes/{id}/policy`. */
  tags(id: string, tags: string[]): R {
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    m.tags = tags;
    this.log('tags' as PoolAuditEntry['action'], m, tags.join(', '));
    return { status: 200, body: m };
  }

  /** `POST /hub/pool/members/{id}/node`. */
  grantNode(id: string, node: boolean): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    if (node && m.state !== 'approved') return { status: 409, body: { error: `${m.id} (${m.name}) is ${m.state}: only an approved member runs lanes` } };
    if (Boolean(m.node) !== node) this.log('node', m, node ? 'may run lanes' : 'runs no lanes');
    if (node) m.node = true;
    else delete m.node;
    return { status: 200, body: this.visible(m) };
  }

  /** `POST /hub/pool/members/{id}/provision`. */
  provision(id: string, on: boolean): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    if (Boolean(m.provision) !== on) this.log('provision', m, on ? 'may cause provisioning' : 'causes no provisioning');
    if (on) m.provision = true;
    else delete m.provision;
    return { status: 200, body: this.visible(m) };
  }

  /** `POST /hub/pool/nodes/{id}/confirm|give-back`. */
  provisioned(id: string, action: string): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const n = this.nodes.find((x) => x.id === id);
    const m = this.members.find((x) => x.id === id);
    if (!n?.provisioned || !m) return { status: 404, body: { error: `\`${id}\` is no provisioned node` } };
    const machine = this.provisioning.machines.find((x) => x.member === id);
    if (action === 'confirm') {
      delete n.provisioned.unconfirmed;
      if (m.provisioned) m.provisioned.confirmed = true;
      this.log('confirm', m, `machine ${n.provisioned.machine}`);
      return { status: 200, body: this.visible(m) };
    }
    this.nodes = this.nodes.filter((x) => x !== n);
    this.leases = this.leases.filter((l) => l.node !== id);
    m.state = 'removed';
    if (machine) machine.state = 'returning';
    this.log('remove', m, 'given back');
    return { status: 202, body: null };
  }

  /** `PUT /hub/pool/provisioned`: this machine's own lanes. */
  setProvisioned(on: boolean): R {
    this.noProvisioned = !on;
    return { status: 200, body: { on } };
  }

  policy(id: string, p: PoolMember['policy'] | null): R {
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: 'no member' } };
    m.policy = p ?? undefined;
    return { status: 200, body: m };
  }

  /** `POST /hub/pool/join`: the machine asks; it waits for an admin. */
  join(): R {
    this.joined = true;
    this.myRole = 'member';
    const mine = this.members.find((x) => x.id === this.me);
    if (mine) mine.state = 'pending';
    return { status: 200, body: { id: this.me, name: 'dev-pc', state: 'pending' } };
  }

  /** `POST /hub/pool/members/{id}/{action}`. */
  act(id: string, action: string, body: { role?: PoolRole } | undefined): R {
    if (this.myRole !== 'admin') return { status: 403, body: { error: 'only an admin of the pool may do this' } };
    const m = this.members.find((x) => x.id === id);
    if (!m) return { status: 404, body: { error: `no member \`${id}\`` } };
    if (action === 'approve') {
      if (m.state === 'removed') return { status: 409, body: { error: `${m.id} (${m.name}) was removed; it joins again with a new key` } };
      m.state = 'approved';
      m.role = body?.role ?? 'member';
      this.nodes.push({ id: m.id, name: m.name, online: false });
      this.log('approve', m, m.role);
    } else if (action === 'remove') {
      if (this.lastAdmin(m)) return { status: 409, body: { error: "the pool's last admin cannot be removed: make another member admin first" } };
      this.log(m.state === 'pending' ? 'reject' : 'remove', m);
      m.state = 'removed';
    } else if (action === 'role') {
      const role = body?.role;
      if (!role) return { status: 400, body: { error: 'name the role: member or admin' } };
      if (role === 'member' && this.lastAdmin(m)) return { status: 409, body: { error: "the pool's last admin stays one: make another member admin first" } };
      m.role = role;
      if (m.id === this.me) this.myRole = role;
      this.log('role', m, role);
    } else return { status: 404, body: { error: 'no route' } };
    return { status: 200, body: this.visible(m) };
  }
}
