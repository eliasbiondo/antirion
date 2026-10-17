import type { Metadata } from 'next';

import { AgentsScreen } from '@/features/agents/components/agents-screen';

export const metadata: Metadata = { title: 'Agents' };

export default function Page() {
  return <AgentsScreen />;
}
