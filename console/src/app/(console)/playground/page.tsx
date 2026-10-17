import type { Metadata } from 'next';

import { PlaygroundScreen } from '@/features/playground/components/playground-screen';

export const metadata: Metadata = { title: 'Playground' };

export default function Page() {
  return <PlaygroundScreen />;
}
