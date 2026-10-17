import type { Metadata } from 'next';

import { CacheScreen } from '@/features/cache/components/cache-screen';

export const metadata: Metadata = { title: 'Cache' };

export default function Page() {
  return <CacheScreen />;
}
