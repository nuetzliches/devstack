/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** "1" when the in-browser fake transport is used (dev:mock, build:mock, VITE_MOCK=1). */
  readonly VITE_MOCK: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare const __APP_VERSION__: string;
