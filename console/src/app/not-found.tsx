import type { Metadata } from 'next';

import { NotFoundScreen } from '@/features/auth/components/not-found-screen';

export const metadata: Metadata = { title: 'Not found' };

export default function NotFound() {
  return <NotFoundScreen />;
}
