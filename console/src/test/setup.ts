import '@testing-library/jest-dom/vitest';

import { cleanup } from '@testing-library/react';
import { afterAll, afterEach, beforeAll, vi } from 'vitest';

import { resetMockState } from '@/mocks/handlers';
import { server } from '@/mocks/node';

// jsdom has no matchMedia; the theme code asks for the system colour scheme.
window.matchMedia = (query: string) =>
  ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    addListener: () => undefined,
    removeListener: () => undefined,
    dispatchEvent: () => false,
  }) as MediaQueryList;

// jsdom has no layout: scrolling an option into view is a no-op there.
Element.prototype.scrollIntoView = () => undefined;

beforeAll(() => server.listen({ onUnhandledFrame: 'error' }));
afterEach(() => {
  cleanup();
  server.resetHandlers();
  resetMockState();
  localStorage.clear();
  // Filters live in the URL: a test starts without the last one's.
  window.history.replaceState(null, '', window.location.pathname);
});
afterAll(() => server.close());

// A navigation re-renders what reads the URL, as in Next.
const listeners = vi.hoisted(() => new Set<() => void>());
const navigation = vi.hoisted(() => ({
  push: vi.fn((url: string) => {
    window.history.pushState(null, '', url);
    for (const l of listeners) l();
  }),
  replace: vi.fn((url: string) => {
    window.history.replaceState(null, '', url);
    for (const l of listeners) l();
  }),
}));
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

const router = {
  push: navigation.push,
  replace: navigation.replace,
  back: vi.fn(),
  refresh: vi.fn(),
  prefetch: vi.fn(),
};
let lastSearch = '';
let lastParams = new URLSearchParams();

// Stable identities between renders, as in Next, so effects that depend on them do not loop.
vi.mock('next/navigation', async () => {
  const { useSyncExternalStore } = await vi.importActual<typeof import('react')>('react');
  return {
    useRouter: () => router,
    usePathname: () => window.location.pathname,
    useSearchParams: () => {
      useSyncExternalStore(
        subscribe,
        () => window.location.search,
        () => '',
      );
      if (window.location.search !== lastSearch) {
        lastSearch = window.location.search;
        lastParams = new URLSearchParams(lastSearch);
      }
      return lastParams;
    },
  };
});

export { navigation };
