import type { Metadata } from 'next';

import { ChangeRequestsScreen } from '@/features/change-requests/components/change-requests-screen';

export const metadata: Metadata = { title: 'Change requests' };

export default function Page() {
  return <ChangeRequestsScreen />;
}
