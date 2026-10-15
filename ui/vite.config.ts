import { fileURLToPath, URL } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { VitePWA } from 'vite-plugin-pwa';

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as { version: string };

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
      VitePWA({
        registerType: 'autoUpdate',
        injectRegister: null,
        includeAssets: ['favicon.svg', 'icon.svg', 'icon-maskable.svg'],
        manifest: {
          name: 'devstack',
          short_name: 'devstack',
          description: 'Dashboard for devstack: services, candidates, logs and resources of your local stacks.',
          start_url: '/',
          scope: '/',
          display: 'standalone',
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
