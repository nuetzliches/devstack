import { fileURLToPath, URL } from 'node:url';
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { defineConfig, type Plugin } from 'vite';
import vue from '@vitejs/plugin-vue';
import { VitePWA } from 'vite-plugin-pwa';

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as {
  version: string;
  dependencies?: Record<string, string>;
};

/** The package a module path below `node_modules` belongs to, if any. */
function packageOf(id: string): string | undefined {
  const parts = id.replace(/\\/g, '/').split('/node_modules/');
  if (parts.length < 2) return undefined;
  const [first, second] = parts[parts.length - 1].split('/');
  return first.startsWith('@') ? `${first}/${second}` : first;
}

/**
 * Writes `bundle-packages.json`: the packages whose code the build puts into
 * `dist`, for the advisory gate (scripts/audit-shipped.mjs, ADR-0111). The
 * service worker is bundled by workbox in a build of its own that this does
 * not see: every installed `workbox-*` package counts as shipped. So do the
 * declared `dependencies`: `vue` ships as `@vue/*`, and an advisory names
 * `vue`.
 */
function bundlePackages(): Plugin {
  const found = new Set<string>();
  return {
    name: 'devstack:bundle-packages',
    apply: 'build',
    generateBundle(_, bundle) {
      for (const chunk of Object.values(bundle)) {
        if (chunk.type !== 'chunk') continue;
        for (const id of Object.keys(chunk.modules)) {
          const name = packageOf(id);
          if (name) found.add(name);
        }
      }
    },
    closeBundle() {
      const modules = fileURLToPath(new URL('./node_modules', import.meta.url));
      for (const name of readdirSync(modules)) {
        if (name.startsWith('workbox-')) found.add(name);
      }
      for (const name of Object.keys(pkg.dependencies ?? {})) found.add(name);
      writeFileSync(
        fileURLToPath(new URL('./bundle-packages.json', import.meta.url)),
        `${JSON.stringify([...found].sort(), null, 2)}\n`,
      );
    },
  };
}

/** The part of the service worker's CacheStorage the shell fallback uses. */
type ShellCaches = { match(url: string, options: { ignoreSearch: boolean }): Promise<Response | undefined> };

export default defineConfig(({ mode }) => {
  // `--mode mock` or VITE_MOCK=1 swaps the real transport for the in-browser fake.
  const mock = mode === 'mock' || process.env.VITE_MOCK === '1';
  return {
    base: '/',
    define: {
      'import.meta.env.VITE_MOCK': JSON.stringify(mock ? '1' : ''),
      __APP_VERSION__: JSON.stringify(pkg.version),
    },
    resolve: {
      alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
    },
    server: {
      // The repository's CHANGELOG.md is bundled for "What's new" (src/lib/changelogText.ts).
      fs: { allow: [fileURLToPath(new URL('..', import.meta.url))] },
      // `npm run dev` against a running hub: forward the API, keep the page on Vite.
      proxy: mock
        ? undefined
        : {
            '/hub': { target: 'http://127.0.0.1:4250', changeOrigin: false, ws: true },
            '/auth': { target: 'http://127.0.0.1:4250', changeOrigin: false },
          },
    },
    plugins: [
      vue(),
      bundlePackages(),
      VitePWA({
        registerType: 'autoUpdate',
        injectRegister: null,
        includeAssets: ['favicon.svg', 'icon.svg', 'icon-maskable.svg'],
        manifest: {
          name: 'devstack',
          short_name: 'devstack',
          description: 'Dashboard for devstack: services, candidates, logs and resources of your local stacks.',
          // The app's id: Chromium names the installed app by its hash, and
          // devstack finds the app by it (ADR-0061).
          id: '/',
          start_url: '/',
          scope: '/',
          display: 'standalone',
          // A launch (the tray, `devstack ui`) navigates the open window
          // instead of opening a second one (ADR-0061).
          launch_handler: { client_mode: 'navigate-existing' },
          background_color: '#0f1115',
          theme_color: '#0f1115',
          icons: [
            { src: 'icon.svg', sizes: 'any', type: 'image/svg+xml', purpose: 'any' },
            { src: 'icon-maskable.svg', sizes: 'any', type: 'image/svg+xml', purpose: 'maskable' },
          ],
        },
        workbox: {
          globPatterns: ['**/*.{js,css,html,svg,woff2}'],
          // The page itself: the hub first, so a reload shows the version it
          // serves now; the cached copy only when the hub does not answer within
          // 2 s. (A precache navigateFallback would always answer from the cache
          // and show a new version only on the second load.)
          navigateFallback: null,
          // Nor may the precache answer `/` with its index.html (it would, before
          // the rule below gets a look); the copy stays for the offline fallback.
          directoryIndex: null,
          runtimeCaching: [
            {
              urlPattern: ({ request, url }) =>
                request.mode === 'navigate' && !url.pathname.startsWith('/hub/') && !url.pathname.startsWith('/auth'),
              handler: 'NetworkFirst',
              options: {
                cacheName: 'devstack-shell',
                networkTimeoutSeconds: 2,
                expiration: { maxEntries: 4 },
                // An error page from a half-started hub must not become the offline copy.
                cacheableResponse: { statuses: [200] },
                // Hub down and no copy of the page yet (first visit since the
                // worker came): the shell from the precache.
                plugins: [
                  {
                    // Runs in the service worker (the build serializes it), where `caches` exists.
                    handlerDidError: async () =>
                      (await (globalThis as unknown as { caches: ShellCaches }).caches.match('/index.html', { ignoreSearch: true })) ??
                      Response.error(),
                  },
                ],
              },
            },
          ],
          cleanupOutdatedCaches: true,
        },
        devOptions: { enabled: false },
      }),
    ],
  };
});
