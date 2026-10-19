// Generic sample stacks for mock mode. No real projects, no real names.
import type { CommandView, ContainerView, GroupView, InfraView, ServiceView, StackEntry, StackView } from '../types';

export function buildId(): string {
  let s = '';
  for (let i = 0; i < 16; i++) s += Math.floor(Math.random() * 16).toString(16);
  return s;
}

const MIN = 60_000;

function service(p: Partial<ServiceView> & Pick<ServiceView, 'name' | 'kind'>, repo: string, now: number): ServiceView {
  const port = p.port ?? null;
  const build = p.build_id === undefined ? buildId() : p.build_id;
  const instance = port ? 49000 + Math.floor(Math.random() * 900) : null;
  const proxied = p.kind === 'service' && port !== null;
  return {
    groups: [],
    autostart: true,
    status: { state: 'healthy' },
    pid: 10000 + Math.floor(Math.random() * 30000),
    since: now - 25 * MIN,
    log: `${repo}\\.devstack\\logs\\${p.name}\\run-${now - 25 * MIN}.log`,
    candidate: null,
    port,
    url: port ? `${p.kind === 'devserver' ? 'https' : 'http'}://localhost:${port}` : null,
    ...p,
    build_id: build,
    instance_port: proxied ? instance : null,
    proxy: proxied
      ? { port: port!, upstream: instance, build, pending: null, active: 2, served: 1840, last_error: null }
      : null,
  } as ServiceView;
}

function container(project: string, svc: string, state: string, health: string, ports: string[]): ContainerView {
  const urls = ports.map((p) => `http://localhost:${p.split(':').pop()}`);
  return { service: svc, name: `${project}-${svc}-1`, state, health, ports, urls };
}

export type MockStackSeed = {
  entry: StackEntry;
  view: StackView;
  /** Previous build per service, for rollback. */
  previous: Record<string, string>;
};

export function shopSeed(now: number): MockStackSeed {
  const repo = 'C:\\work\\shop';
  const services: ServiceView[] = [
    service({ name: 'api', kind: 'service', groups: ['app'], port: 5080, hot_reload_available: true }, repo, now),
    service({ name: 'worker', kind: 'service', groups: ['app'] }, repo, now),
    service({ name: 'web', kind: 'devserver', groups: ['app'], port: 4200 }, repo, now),
    service(
      {
        name: 'migrate',
        kind: 'job',
        groups: ['data'],
        autostart: false,
        status: { state: 'completed', exit_code: 0 },
        pid: null,
      },
      repo,
      now,
    ),
    service(
      {
        name: 'mailbox',
        kind: 'tool',
        groups: [],
        port: 8025,
        autostart: false,
        status: { state: 'stopped' },
        pid: null,
      },
      repo,
      now,
    ),
  ];
  const infra: InfraView[] = [
    {
      group: 'data',
      project: 'shop',
      containers: [
        container('shop', 'db', 'running', 'healthy', ['5432->127.0.0.1:15432']),
        container('shop', 'cache', 'running', '', ['6379->127.0.0.1:16379']),
        container('shop', 'queue', 'running', 'healthy', ['5672->127.0.0.1:15672', '15672->127.0.0.1:25672']),
      ],
      error: null,
    },
    {
      group: 'observability',
      project: 'shop-obs',
      containers: [container('shop-obs', 'collector', 'running', '', ['4317->127.0.0.1:14317'])],
      error: null,
    },
  ];
  const groups: GroupView[] = [
    { name: 'app', label: 'Application', order: 1, members: ['api', 'worker', 'web', 'command.seed'] },
    {
      name: 'data',
      label: 'Data',
      order: 2,
      members: ['compose.data.db', 'compose.data.cache', 'compose.data.queue', 'migrate', 'command.reset-db'],
    },
  ];
  const commands: CommandView[] = [
    { name: 'seed', description: 'Load demo customers, products and orders through the API', confirm: null, requires: ['api'] },
    {
      name: 'reset-db',
      description: 'Drop the development database, run all migrations and seed it again',
      confirm: 'This drops the development database "shop" and recreates it. All local data is lost. Continue?',
      requires: ['db'],
    },
    { name: 'lint', description: 'Run the linters over the working tree', confirm: null, requires: [] },
  ];
  const previous: Record<string, string> = {};
  for (const s of services) previous[s.name] = buildId();
  return {
    entry: {
      id: 'shop-3f9a1c',
      name: 'shop',
      file: `${repo}\\devstack.toml`,
      repo,
      running: true,
      daemon: { pid: 21544, port: 52711, started: now - 26 * MIN, version: '0.2.0' },
      last_seen: now,
    },
    view: {
      name: 'shop',
      repo,
      branch: 'main',
      version: '0.2.0',
      services,
      infra,
      groups,
      commands,
      profiles: [{ name: 'acceptance', description: 'the end-to-end suite on a build', fresh: true, command: 'seed' }],
    },
    previous,
  };
}

export function crmSeed(now: number): MockStackSeed {
  const repo = '/home/dev/src/crm';
  const services: ServiceView[] = [
    service({ name: 'gateway', kind: 'service', groups: ['edge'], port: 7000 }, repo, now),
    service({ name: 'contacts', kind: 'service', groups: ['core'], port: 7010 }, repo, now),
    service(
      {
        name: 'mailer',
        kind: 'service',
        groups: ['core'],
        port: 7020,
        status: { state: 'unhealthy', reason: 'health check GET /health returned 503 three times in a row' },
      },
      repo,
      now,
    ),
    service({ name: 'portal', kind: 'devserver', groups: ['edge'], port: 3000 }, repo, now),
  ];
  const infra: InfraView[] = [
    {
      group: 'core',
      project: 'crm',
      containers: [
        container('crm', 'postgres', 'running', 'healthy', ['5432->127.0.0.1:25432']),
        container('crm', 'search', 'running', 'starting', ['9200->127.0.0.1:29200']),
      ],
      error: null,
    },
  ];
  const groups: GroupView[] = [
    { name: 'edge', label: 'Edge', order: 1, members: ['gateway', 'portal'] },
    { name: 'core', label: 'Core services', order: 2, members: ['contacts', 'mailer', 'compose.core.postgres', 'compose.core.search', 'command.reindex'] },
  ];
  const commands: CommandView[] = [
    { name: 'reindex', description: 'Rebuild the contact search index', confirm: null, requires: ['contacts'] },
  ];
  const previous: Record<string, string> = {};
  for (const s of services) previous[s.name] = buildId();
  return {
    entry: {
      id: 'crm-81c0d2',
      name: 'crm',
      file: `${repo}/devstack.toml`,
      repo,
      running: true,
      daemon: { pid: 4410, port: 41877, started: now - 95 * MIN, version: '0.2.0' },
      last_seen: now,
    },
    view: { name: 'crm', repo, version: '0.2.0', services, infra, groups, commands },
    previous,
  };
}

export function docsSeed(now: number): MockStackSeed {
  const repo = 'C:\\work\\docs';
  const last = now - 2 * 60 * MIN - 14 * MIN;
  const services: ServiceView[] = [
    service({ name: 'site', kind: 'devserver', groups: ['docs'], port: 8080, since: last - 40 * MIN }, repo, now),
    service({ name: 'search-api', kind: 'service', groups: ['docs'], port: 8090, since: last - 40 * MIN }, repo, now),
  ];
  const infra: InfraView[] = [
    { group: 'docs', project: 'docs', containers: [container('docs', 'meili', 'running', 'healthy', ['7700->127.0.0.1:17700'])], error: null },
  ];
  const groups: GroupView[] = [
    { name: 'docs', label: 'Documentation', order: null, members: ['site', 'search-api', 'compose.docs.meili', 'command.build-pdf'] },
  ];
  const commands: CommandView[] = [
    { name: 'build-pdf', description: 'Render the handbook as PDF', confirm: null, requires: ['site'] },
  ];
  const previous: Record<string, string> = {};
  for (const s of services) previous[s.name] = buildId();
  return {
    entry: {
      id: 'docs-07be44',
      name: 'docs',
      file: `${repo}\\devstack.toml`,
      repo,
      running: false,
      daemon: null,
      last_seen: last,
    },
    view: { name: 'docs', repo, version: '0.2.0', services, infra, groups, commands },
    previous,
  };
}

// ---------------------------------------------------------------- log lines

const paths = ['/api/orders', '/api/products', '/api/customers/42', '/api/cart', '/health', '/api/orders/1093/items'];
const methods = ['GET', 'GET', 'GET', 'POST', 'PUT', 'DELETE'];

function pick<T>(xs: T[]): T {
  return xs[Math.floor(Math.random() * xs.length)];
}

function ts(): string {
  const d = new Date();
  const p = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

export const stamp = ts;

export function logLine(service: string, kind: string): string {
  const r = Math.random();
  if (kind === 'devserver') {
    if (r < 0.08) return `${ts()} [vite] warning: "./legacy/format.ts" is imported by both a dynamic and a static import`;
    // A dev server colours its output for a terminal (rendered by the log view).
    if (r < 0.2) return `\u001b[33m▲ \u001b[43;33m[\u001b[43;30mWARNING\u001b[43;33m]\u001b[0m \u001b[1mNG8113: NeoButtonComponent is not used within the template\u001b[0m \u001b[1m\u001b[35m[plugin angular-compiler]\u001b[0m`;
    if (r < 0.5) return `${ts()} [vite] hmr update /src/components/${pick(['Cart', 'ProductCard', 'Header', 'Checkout'])}.vue`;
    return `${ts()} [vite] page reload src/${pick(['main.ts', 'router.ts', 'App.vue'])}`;
  }
  if (service === 'worker' || service === 'mailer') {
    if (r < 0.04) return `${ts()} fail: Worker.Dispatcher[3] Message 7f1c could not be delivered: connection reset by peer (retry 1/5)`;
    if (r < 0.12) return `${ts()} warn: Worker.Dispatcher[2] Queue depth 140 exceeds soft limit 100`;
    return `${ts()} info: Worker.Processor[1] Processed batch of ${2 + Math.floor(Math.random() * 30)} messages in ${20 + Math.floor(Math.random() * 120)} ms`;
  }
  if (r < 0.015) return `${ts()} fail: App.Errors[500] Unhandled exception for ${pick(methods)} ${pick(paths)}: TimeoutException: The operation has timed out.`;
  if (r < 0.1) return `${ts()} warn: App.Http[429] Slow request ${pick(methods)} ${pick(paths)} took ${800 + Math.floor(Math.random() * 900)} ms`;
  if (r < 0.14) return `${ts()} dbug: App.Data[10] Executed query in ${1 + Math.floor(Math.random() * 9)} ms`;
  const status = Math.random() < 0.92 ? 200 : pick([201, 204, 404]);
  return `${ts()} info: App.Http[2] ${pick(methods)} ${pick(paths)} -> ${status} in ${(2 + Math.random() * 40).toFixed(1)} ms`;
}

export function startupLines(service: string, port: number | null): string[] {
  return [
    `${ts()} info: App.Host[0] Starting ${service} (environment: Development)`,
    `${ts()} info: App.Host[0] Configuration loaded from compose (3 values)`,
    port ? `${ts()} info: App.Host[14] Now listening on: http://127.0.0.1:${port}` : `${ts()} info: App.Host[0] Background workers disabled until promotion`,
    `${ts()} info: App.Host[0] Application started. Press Ctrl+C to shut down.`,
  ];
}

export const failedReason = [
  'build failed with 2 errors (the running instance stays):',
  'src/Api/Program.cs(12,5): error CS1002: ; expected',
  "src/Api/Endpoints/OrderEndpoints.cs(48,27): error CS0103: The name 'orderId' does not exist in the current context",
].join('\n');

export function buildLog(service: string, failed: boolean): string {
  const lines = [
    `devstack build ${service} (snapshot 9c2e41f0, lane main)`,
    '  Determining projects to restore...',
    '  All projects are up-to-date for restore.',
    '  Shared -> C:\\work\\shop\\.devstack\\builds\\9c2e41f0\\Shared.dll',
    '  Data -> C:\\work\\shop\\.devstack\\builds\\9c2e41f0\\Data.dll',
  ];
  if (failed) {
    lines.push(
      "src/Api/Program.cs(12,5): error CS1002: ; expected [C:\\work\\shop\\src\\Api\\Api.csproj]",
      "src/Api/Endpoints/OrderEndpoints.cs(48,27): error CS0103: The name 'orderId' does not exist in the current context [C:\\work\\shop\\src\\Api\\Api.csproj]",
      "src/Api/Endpoints/OrderEndpoints.cs(31,9): warning CS8602: Dereference of a possibly null reference. [C:\\work\\shop\\src\\Api\\Api.csproj]",
      '',
      'Build FAILED.',
      '',
      '    1 Warning(s)',
      '    2 Error(s)',
      '',
      'Time Elapsed 00:00:04.81',
    );
  } else {
    lines.push(
      `  ${service} -> C:\\work\\shop\\.devstack\\builds\\9c2e41f0\\${service}.dll`,
      '',
      'Build succeeded.',
      '    0 Warning(s)',
      '    0 Error(s)',
      '',
      'Time Elapsed 00:00:05.12',
    );
  }
  return lines.join('\n');
}
