import type { Metadata } from 'next';

import { SettingsRedirect } from '@/features/settings/components/settings-redirect';

export const metadata: Metadata = { title: 'Gateway settings' };

/** Settings is a dialog now; this address opens it over the Overview. */
export default function Page() {
  return <SettingsRedirect />;
}
