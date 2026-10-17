import type { NextConfig } from 'next';
import { PHASE_DEVELOPMENT_SERVER } from 'next/constants';

/**
 * The console is a static export served by the gateway's admin listener under `admin.ui.path`.
 * `basePath` and `assetPrefix` must equal that path; the default is `/ui`. Build for another path
 * with `ANTIRION_UI_PATH=/console pnpm build`.
 */
const uiPath = normalisePath(process.env.ANTIRION_UI_PATH ?? '/ui');

/** In development the Admin API is proxied from this address (a gateway's admin listener). */
const devAdminUrl = process.env.ANTIRION_DEV_ADMIN_URL ?? 'http://127.0.0.1:20481';

function normalisePath(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  if (trimmed === '') return '';
  return trimmed.startsWith('/') ? trimmed : `/${trimmed}`;
}

/** `pnpm dev:mock` only: the mock worker for contract-only endpoints; other builds alias it away. */
const mocks = process.env.NEXT_PUBLIC_API_MOCKS === '1';

export default function config(phase: string): NextConfig {
  const dev = phase === PHASE_DEVELOPMENT_SERVER;
  return {
    turbopack: {
      resolveAlias: mocks
        ? { 'msw/browser': './node_modules/msw/lib/browser/index.js' }
        : { '@/mocks/browser': './src/mocks/disabled.ts' },
    },
    agentRules: false,
    output: 'export',
    // A release names its commit, so the export (and the binary embedding it) is reproducible; other
    // builds keep a fresh id, since the build id names immutable asset paths.
    generateBuildId: () => Promise.resolve(process.env.ANTIRION_BUILD_COMMIT ?? null),
    basePath: uiPath,
    assetPrefix: uiPath,
    trailingSlash: true,
    reactStrictMode: true,
    poweredByHeader: false,
    productionBrowserSourceMaps: false,
    images: { unoptimized: true },
    env: { NEXT_PUBLIC_UI_PATH: uiPath },
    typedRoutes: false,
    ...(dev
      ? {
          // Compressing the proxied Admin API would buffer the request log's live tail.
          compress: false,
          // The Admin API takes no trailing slash: without this, the proxied calls would be redirected
          // to `…/rules/` and refused. Pages still answer on their trailing-slash addresses.
          skipTrailingSlashRedirect: true,
          rewrites: () =>
            Promise.resolve([
              {
                source: '/admin/v1/:path*',
                destination: `${devAdminUrl}/admin/v1/:path*`,
                basePath: false as const,
              },
            ]),
        }
      : {}),
  };
}
