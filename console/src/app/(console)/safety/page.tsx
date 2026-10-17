import type { Metadata } from 'next';

import { SafetyScreen } from '@/features/safety/components/safety-screen';

export const metadata: Metadata = { title: 'Safety & PII' };

export default function Page() {
  return <SafetyScreen />;
}
