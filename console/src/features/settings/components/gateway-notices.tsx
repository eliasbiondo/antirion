'use client';

import Link from 'next/link';

import { useSettingsHref } from '@/components/layout/url-sync';
import { Banner } from '@/components/ui/states';
import { formatDateTime } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useShell } from '@/stores/shell';

/**
 * Gateway-wide states every page should show while they hold: a dry run of every request, a
 * gateway-wide traffic pause and a maintenance window that is on now. Each links to the danger zone.
 */
export function GatewayNotices() {
  const status = useShell((s) => s.status);
  const timeZone = usePreferences((s) => s.timeZone);
  const dangerHref = useSettingsHref('danger');
  if (!status) return null;
  const pause = (status.ingress_pauses ?? []).find((p) => p.organization === null);
  const window = status.maintenance_window?.active ? status.maintenance_window : null;
  const link = (text: string) => (
    <Link href={dangerHref} className="text-ui font-medium text-accent hover:underline">
      {text}
    </Link>
  );
  return (
    <>
      {status.dry_run_all ? (
        <Banner tone="warn" title="Dry run is on for every request" actions={link('Turn off')}>
          Apps get the model the gateway would choose and its estimated cost, not real answers. Nothing is
          sent to providers.
        </Banner>
      ) : null}
      {pause ? (
        <Banner
          tone="error"
          title={`All traffic is paused until ${formatDateTime(pause.resumes_at, timeZone)}`}
          actions={link('Resume')}
        >
          Apps get an error for new requests. {pause.reason}
        </Banner>
      ) : null}
      {window ? (
        <Banner tone="info" title={`Maintenance until ${formatDateTime(window.end, timeZone)}`}>
          {window.description ?? 'Planned work is in progress.'}
        </Banner>
      ) : null}
    </>
  );
}
