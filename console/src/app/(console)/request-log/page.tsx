import type { Metadata } from 'next';

import { RequestLogScreen } from '@/features/request-log/components/request-log-screen';

export const metadata: Metadata = { title: 'Request log' };

export default function Page() {
  return <RequestLogScreen />;
}
