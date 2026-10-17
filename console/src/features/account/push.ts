import { api } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';

export type PushSubscriptionView = components['schemas']['PushSubscription'];

/** Where the console's service worker is served and what it controls. */
const UI_PATH = process.env.NEXT_PUBLIC_UI_PATH ?? '';
const WORKER = `${UI_PATH}/push-worker.js`;
const SCOPE = `${UI_PATH}/`;

/** What this browser can do with push notifications. */
export type BrowserPush = 'unsupported' | 'denied' | 'off' | 'on';

export function pushSupported(): boolean {
  return (
    typeof window !== 'undefined' &&
    window.isSecureContext &&
    'serviceWorker' in navigator &&
    'PushManager' in window &&
    'Notification' in window
  );
}

async function registration(): Promise<ServiceWorkerRegistration> {
  const existing = await navigator.serviceWorker.getRegistration(SCOPE);
  return existing ?? navigator.serviceWorker.register(WORKER, { scope: SCOPE });
}

/** This browser's state, and its current subscription's endpoint when it has one. */
export async function browserPush(): Promise<{ state: BrowserPush; endpoint: string | null }> {
  if (!pushSupported()) return { state: 'unsupported', endpoint: null };
  if (Notification.permission === 'denied') return { state: 'denied', endpoint: null };
  const reg = await navigator.serviceWorker.getRegistration(SCOPE);
  const sub = await reg?.pushManager.getSubscription();
  return sub ? { state: 'on', endpoint: sub.endpoint } : { state: 'off', endpoint: null };
}

function keyBytes(base64url: string): Uint8Array<ArrayBuffer> {
  const base64 = base64url.replace(/-/g, '+').replace(/_/g, '/');
  const raw = atob(base64 + '='.repeat((4 - (base64.length % 4)) % 4));
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
  return out;
}

/** What this browser calls itself in the list of browsers: its platform and browser family. */
export function deviceName(ua: string = typeof navigator === 'undefined' ? '' : navigator.userAgent): string {
  const browser = ua.includes('Edg/')
    ? 'Edge'
    : ua.includes('Firefox/')
      ? 'Firefox'
      : ua.includes('Chrome/')
        ? 'Chrome'
        : ua.includes('Safari/')
          ? 'Safari'
          : 'Browser';
  const platform = /Mac OS X|Macintosh/.test(ua)
    ? 'macOS'
    : ua.includes('Windows')
      ? 'Windows'
      : ua.includes('Android')
        ? 'Android'
        : /iPhone|iPad/.test(ua)
          ? 'iOS'
          : ua.includes('Linux')
            ? 'Linux'
            : null;
  return platform ? `${browser} on ${platform}` : browser;
}

/** This browser's subscription id at the gateway, kept in this browser only to name its own row. */
const OWN = 'antirion.console.push-subscription';

export function ownSubscription(): string | null {
  try {
    return localStorage.getItem(OWN);
  } catch {
    return null;
  }
}

function keepOwn(id: string | null) {
  try {
    if (id) localStorage.setItem(OWN, id);
    else localStorage.removeItem(OWN);
  } catch {
    // Without storage the console only cannot tell which row is this browser's.
  }
}

export const pushApi = {
  overview: () => api.get('/auth/me/push'),
  remove: (id: string) =>
    api.delete('/auth/me/push/subscriptions/{subscription_id}', { path: { subscription_id: id } }),
  test: () => api.post('/auth/me/push:test'),

  /** Asks the browser's permission, subscribes it with the gateway's key and registers it. */
  async turnOn(publicKey: string): Promise<PushSubscriptionView> {
    const permission = await Notification.requestPermission();
    if (permission !== 'granted') throw new Error('Notifications are blocked for this site in the browser.');
    const reg = await registration();
    await navigator.serviceWorker.ready;
    const sub =
      (await reg.pushManager.getSubscription()) ??
      (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(publicKey) }));
    const json = sub.toJSON() as { endpoint?: string; keys?: { p256dh?: string; auth?: string } };
    const created = await api.post('/auth/me/push/subscriptions', {
      body: {
        endpoint: json.endpoint ?? sub.endpoint,
        keys: { p256dh: json.keys?.p256dh ?? '', auth: json.keys?.auth ?? '' },
        device: deviceName(),
      },
    });
    keepOwn(created.id);
    return created;
  },

  /** Unsubscribes this browser and removes its subscription from the gateway. */
  async turnOff(id: string | null): Promise<void> {
    const reg = await navigator.serviceWorker.getRegistration(SCOPE);
    const sub = await reg?.pushManager.getSubscription();
    await sub?.unsubscribe();
    if (id) await pushApi.remove(id);
    keepOwn(null);
  },
};
