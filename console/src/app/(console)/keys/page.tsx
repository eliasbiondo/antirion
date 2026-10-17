import type { Metadata } from 'next';

import { KeysScreen } from '@/features/keys/components/keys-screen';

export const metadata: Metadata = { title: 'API keys' };

export default function Page() {
  return <KeysScreen />;
}
