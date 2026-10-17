import type { Metadata } from 'next';
import { Suspense } from 'react';

import { ConfirmEmailScreen } from '@/features/auth/components/confirm-email-screen';

export const metadata: Metadata = { title: 'Confirm email' };

/** Reached from the link mailed to a new email address (`?token=`). */
export default function Page() {
  return (
    <Suspense fallback={null}>
      <ConfirmEmailScreen />
    </Suspense>
  );
}
