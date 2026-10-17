import type { Metadata } from 'next';

import { RoutingScreen } from '@/features/routing/components/routing-screen';

export const metadata: Metadata = { title: 'Routing policy' };

export default function Page() {
  return <RoutingScreen />;
}
