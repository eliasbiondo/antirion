import type { Metadata } from 'next';

import { AlertsScreen } from '@/features/alerts/components/alerts-screen';

export const metadata: Metadata = { title: 'Alerts' };

export default function Page() {
  return <AlertsScreen />;
}
