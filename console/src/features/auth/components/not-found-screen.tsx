import Link from 'next/link';

import { BrandMark } from '@/components/ui/icon';

/** Unknown console paths: a static page, the same for everyone. */
export function NotFoundScreen() {
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-3 bg-canvas px-6 text-center">
      <span className="mb-2 text-accent">
        <BrandMark size={28} />
      </span>
      <h1 className="text-heading font-semibold tracking-tight text-fg">Page not found</h1>
      <p className="max-w-md text-body text-muted">The link may be out of date, or the page has moved.</p>
      <Link
        href="/"
        className="mt-3 inline-flex h-9 items-center rounded-sm border border-accent bg-accent px-4 text-ui font-medium text-accent-fg hover:brightness-110"
      >
        Go to Overview
      </Link>
    </main>
  );
}
