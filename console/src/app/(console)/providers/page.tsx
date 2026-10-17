import type { Metadata } from 'next';

import { ProvidersScreen } from '@/features/providers/components/providers-screen';

export const metadata: Metadata = { title: 'Providers' };

export default function Page() {
  return <ProvidersScreen />;
}
