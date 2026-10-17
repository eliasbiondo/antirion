import type { Metadata } from 'next';
import { Suspense } from 'react';

import { LoginScreen } from '@/features/auth/components/login-screen';

export const metadata: Metadata = { title: 'Accept invitation' };

/** Reached from the emailed link (`?token=`). */
export default function Page() {
  return (
    <Suspense fallback={null}>
      <LoginScreen entry="invitation" />
    </Suspense>
  );
}
