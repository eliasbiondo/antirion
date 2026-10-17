'use client';

import { usePathname } from 'next/navigation';
import { useEffect } from 'react';

import { alertsApi } from '@/features/alerts/api';
import { ChannelDrawer } from '@/features/alerts/components/channel-drawer';
import { WebhookDrawer } from '@/features/alerts/components/webhook-drawer';
import { useAlertEditors } from '@/features/alerts/editors';
import { useAlerts } from '@/features/alerts/store';
import { loadSlot } from '@/lib/slot';

/**
 * Reads what the Alerts editors need (rules, channels, event webhooks and their delivery status),
 * without the alert history the Alerts screen also loads.
 */
export function loadAlertConfig(options?: { quiet?: boolean }): Promise<void> {
  const set = useAlerts.setState;
  const get = useAlerts.getState;
  return Promise.all([
    loadSlot(set, get, 'rules', alertsApi.rules, options),
    loadSlot(set, get, 'channels', alertsApi.channels, options),
    loadSlot(set, get, 'channelStatus', alertsApi.channelStatus, options),
    loadSlot(set, get, 'webhooks', alertsApi.webhooks, options),
    loadSlot(set, get, 'webhookStatus', alertsApi.webhookStatus, options),
  ]).then(() => undefined);
}

/** Loads the alert configuration once a section that shows it is on screen. */
export function useAlertConfig(): void {
  useEffect(() => {
    void loadAlertConfig();
  }, []);
}

/**
 * The channel and event webhook editors of Alerts, opened over Settings. On the Alerts screen its
 * own host draws them, so this one stays empty there.
 */
export function AlertEditorsHost() {
  const stack = useAlertEditors((s) => s.stack);
  const pathname = usePathname();
  useEffect(() => () => useAlertEditors.setState({ stack: [] }), []);
  if (pathname.startsWith('/alerts')) return null;
  return (
    <>
      {stack.map((e, i) => {
        const key = `${e.kind}-${i}`;
        if (e.kind === 'channel') return <ChannelDrawer key={key} name={e.name} webhook={e.webhook} />;
        if (e.kind === 'webhook') return <WebhookDrawer key={key} name={e.name} events={e.events} />;
        return null;
      })}
    </>
  );
}
