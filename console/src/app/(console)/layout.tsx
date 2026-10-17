import { Suspense, type ReactNode } from 'react';

import { AppShell, Splash } from '@/components/layout/app-shell';

export default function ConsoleLayout({ children }: { children: ReactNode }) {
  return (
    <Suspense fallback={<Splash />}>
      <AppShell>{children}</AppShell>
    </Suspense>
  );
}
