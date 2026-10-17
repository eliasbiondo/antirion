import { setupWorker } from 'msw/browser';

import { contractHandlers, demoHandlers } from './handlers';

let started: Promise<void> | null = null;

/** The development worker: contract-only operations are mocked, everything else reaches the gateway. */
export function startMockWorker(): Promise<void> {
  started ??= start();
  return started;
}

async function start(): Promise<void> {
  const basePath = process.env.NEXT_PUBLIC_UI_PATH ?? '';
  await setupWorker(...contractHandlers, ...demoHandlers).start({
    serviceWorker: { url: `${basePath}/mockServiceWorker.js` },
    onUnhandledFrame: 'bypass',
    quiet: true,
  });
}
