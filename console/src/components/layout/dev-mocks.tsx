'use client';

import { useEffect, useState, type ReactNode } from 'react';

const MOCKS = process.env.NEXT_PUBLIC_API_MOCKS === '1';

/**
 * In `pnpm dev:mock`, holds the page until the mock worker for contract-only endpoints is running.
 * In every other build `MOCKS` is false at compile time and this renders its children directly.
 */
export function DevMocks({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(!MOCKS);
  useEffect(() => {
    if (!MOCKS) return;
    void import('@/mocks/browser').then((m) => m.startMockWorker()).finally(() => setReady(true));
  }, []);
  return ready ? children : null;
}
