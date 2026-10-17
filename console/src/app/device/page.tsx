import type { Metadata } from 'next';
import { Suspense } from 'react';

import { DeviceScreen } from '@/features/auth/components/device-screen';

export const metadata: Metadata = { title: 'Approve a terminal sign-in' };

/** Opened from the link `antirion auth login` prints (`?code=`). */
export default function Page() {
  return (
    <Suspense fallback={null}>
      <DeviceScreen />
    </Suspense>
  );
}
