import type { Metadata } from 'next';

import { PluginsScreen } from '@/features/plugins/components/plugins-screen';

export const metadata: Metadata = { title: 'Plugins' };

export default function Page() {
  return <PluginsScreen />;
}
