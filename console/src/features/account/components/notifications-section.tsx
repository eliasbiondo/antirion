'use client';

import { useCallback, useEffect, useState } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Checkbox, Field, Segmented } from '@/components/ui/form';
import { EmptyState, ErrorState, InlineError, isUnavailable, LoadingRows } from '@/components/ui/states';
import { api } from '@/lib/api/client';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';
import { usePermission } from '@/components/ui/gate';

import { type BrowserPush, browserPush, ownSubscription, pushApi, type PushSubscriptionView } from '../push';

type Digest = 'off' | 'daily' | 'weekly';
type Channel = 'email' | 'push';

const SEVERITIES = ['critical', 'warning', 'info'] as const;
const CATEGORIES = ['availability', 'latency', 'budget', 'safety', 'capacity', 'cache', 'anomaly'] as const;
const CHANNELS: Channel[] = ['email', 'push'];

const SEVERITY_LABEL: Record<(typeof SEVERITIES)[number], string> = {
  critical: 'Critical',
  warning: 'Warning',
  info: 'Info',
};

const CATEGORY_LABEL: Record<(typeof CATEGORIES)[number], string> = {
  availability: 'Availability',
  latency: 'Latency',
  budget: 'Budgets',
  safety: 'Safety',
  capacity: 'Capacity',
  cache: 'Cache',
  anomaly: 'Unusual activity',
};

const CHANNEL_LABEL: Record<Channel, string> = { email: 'Email', push: 'Push' };

/** One channel's choice, as the gateway reads it: the severities, and the categories (all of them when empty). */
interface Choice {
  severities: string[];
  categories: string[];
}

interface NotificationPrefs {
  digest: Digest;
  email: Choice;
  push: Choice;
}

const ALL_CATEGORIES: string[] = [...CATEGORIES];

/** A channel's stored choice; an earlier per-cell map (`"budget.critical": true`) is read as its rows and columns. */
function readChoice(value: unknown): Choice {
  if (!value || typeof value !== 'object') return { severities: [], categories: ALL_CATEGORIES };
  const v = value as Record<string, unknown>;
  if (Array.isArray(v.severities) || Array.isArray(v.categories)) {
    const list = (x: unknown) =>
      Array.isArray(x) ? x.filter((s): s is string => typeof s === 'string') : [];
    const categories = list(v.categories);
    return {
      severities: list(v.severities),
      categories: categories.length === 0 ? ALL_CATEGORIES : categories,
    };
  }
  const on = Object.entries(v)
    .filter(([, x]) => x === true)
    .map(([k]) => k.split('.'));
  return {
    severities: [...new Set(on.map(([, s]) => s ?? ''))].filter(Boolean),
    categories: on.length ? [...new Set(on.map(([c]) => c ?? ''))].filter(Boolean) : ALL_CATEGORIES,
  };
}

function readPrefs(value: unknown): NotificationPrefs {
  const v = (value ?? {}) as { digest?: unknown; email?: unknown; push?: unknown };
  const digest = v.digest === 'daily' || v.digest === 'weekly' ? v.digest : 'off';
  return { digest, email: readChoice(v.email), push: readChoice(v.push) };
}

/** A choice as stored: every category is written as none listed; no category at all turns the channel off. */
function written(c: Choice): Choice {
  if (c.categories.length === 0) return { severities: [], categories: [] };
  const all = ALL_CATEGORIES.every((x) => c.categories.includes(x));
  return { severities: c.severities, categories: all ? [] : c.categories };
}

function toggled(list: string[], value: string): string[] {
  return list.includes(value) ? list.filter((x) => x !== value) : [...list, value];
}

/** This browser's push state and the browsers that receive the user's notifications. */
function usePush() {
  const [version, setVersion] = useState(0);
  const [state, setState] = useState<{
    subs: PushSubscriptionView[] | null;
    key: string | null;
    error: unknown;
    here: { state: BrowserPush; endpoint: string | null };
  }>({ subs: null, key: null, error: null, here: { state: 'off', endpoint: null } });
  useEffect(() => {
    let cancelled = false;
    Promise.all([pushApi.overview(), browserPush()])
      .then(([overview, here]) => {
        if (!cancelled)
          setState({ subs: overview.subscriptions, key: overview.public_key, error: null, here });
      })
      .catch((error: unknown) => {
        if (!cancelled) setState((s) => ({ ...s, error }));
      });
    return () => {
      cancelled = true;
    };
  }, [version]);
  const reload = useCallback(() => setVersion((v) => v + 1), []);
  return { ...state, reload };
}

function BrowsersPanel({ push }: { push: ReturnType<typeof usePush> }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const { subs, key, here, reload } = push;
  if (push.error) {
    if (isUnavailable(push.error)) return null;
    return <ErrorState error={push.error} subject="push notifications" size="sm" onRetry={reload} />;
  }
  if (!subs) return <LoadingRows rows={2} />;

  async function run(label: string, action: () => Promise<void>) {
    setBusy(label);
    setError(null);
    try {
      await action();
      reload();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  }

  const ownId = here.state === 'on' ? ownSubscription() : null;
  const mine = subs.find((s) => s.id === ownId) ?? null;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 flex-col">
          <span className="text-body font-semibold text-fg">Browsers that get push notifications</span>
          <span className="text-caption text-muted">
            {here.state === 'unsupported'
              ? 'This browser cannot receive notifications from the console.'
              : here.state === 'denied'
                ? 'Notifications are blocked for this site in this browser’s settings.'
                : here.state === 'on'
                  ? 'This browser gets them.'
                  : 'This browser does not get them yet.'}
          </span>
        </div>
        <div className="flex items-center gap-2">
          {subs.length > 0 ? (
            <Button
              variant="ghost"
              busy={busy === 'test'}
              onClick={() =>
                void run('test', async () => {
                  const r = await pushApi.test();
                  const sent = r.results.filter((x) => x.ok).length;
                  notify(
                    sent === r.results.length ? 'success' : 'warn',
                    sent === r.results.length ? 'Test notification sent' : 'Some browsers did not get it',
                    `${sent} of ${r.results.length}`,
                  );
                })
              }
            >
              Send a test
            </Button>
          ) : null}
          {here.state === 'off' && key ? (
            <Button
              variant="secondary"
              icon="bell"
              busy={busy === 'on'}
              onClick={() =>
                void run('on', async () => {
                  await pushApi.turnOn(key);
                  notify('success', 'Push notifications are on in this browser');
                })
              }
            >
              Turn on in this browser
            </Button>
          ) : null}
          {here.state === 'on' ? (
            <Button
              variant="ghost"
              busy={busy === 'off'}
              onClick={() => void run('off', () => pushApi.turnOff(mine?.id ?? null))}
            >
              Turn off in this browser
            </Button>
          ) : null}
        </div>
      </div>
      <InlineError error={error} />
      {subs.length === 0 ? (
        <EmptyState size="sm" title="No browser gets your notifications yet" />
      ) : (
        <ul aria-label="Browsers that get push notifications" className="flex flex-col">
          {subs.map((s) => (
            <li key={s.id} className="flex items-center gap-3 border-t border-line py-2 first:border-t-0">
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate text-ui text-fg">
                  {s.device ?? 'Browser'}
                  {mine?.id === s.id ? (
                    <span className="text-caption text-muted"> · this browser</span>
                  ) : null}
                </span>
                <span className="mono truncate text-caption text-muted">
                  {s.origin.replace(/^https:\/\//, '')}
                  {' · '}
                  {s.last_error
                    ? s.last_error
                    : s.last_success_at
                      ? `last delivered ${formatRelative(s.last_success_at)}`
                      : `added ${formatRelative(s.created_at)}`}
                </span>
              </span>
              <IconButton
                icon="trash"
                label={`Remove ${s.device ?? 'this browser'}`}
                onClick={() => void run(`rm-${s.id}`, () => pushApi.remove(s.id).then(() => undefined))}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Personal notifications: digest frequency, email and browser push per alert severity and category. */
export function NotificationsSection() {
  const [prefs, setPrefs] = useState<NotificationPrefs | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const push = usePush();
  const pushOffered = !push.error || !isUnavailable(push.error);
  const canAlerts = usePermission('alerts:read');

  useEffect(() => {
    if (!canAlerts) return;
    api
      .get('/auth/me/preferences')
      .then((p) => setPrefs(readPrefs(p.notifications)))
      .catch(setLoadError);
  }, [canAlerts]);

  if (!canAlerts)
    return (
      <EmptyState
        title="No alerts to notify you about"
        description="Your role doesn't include alerts. Ask a gateway admin if you should get them."
      />
    );

  if (loadError)
    return (
      <ErrorState
        error={loadError}
        subject="notification settings"
        onRetry={() => window.location.reload()}
      />
    );
  if (!prefs) return <LoadingRows rows={4} />;
  const channels = CHANNELS.filter((c) => c === 'email' || pushOffered);

  async function save(current: NotificationPrefs) {
    setBusy(true);
    setError(null);
    try {
      const existing = await api.get('/auth/me/preferences');
      const notifications: Record<string, unknown> = {
        digest: current.digest,
        email: written(current.email),
      };
      if (pushOffered) notifications.push = written(current.push);
      await api.put('/auth/me/preferences', { body: { ...existing, notifications } });
      notify('success', 'Notification settings saved');
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const set = (channel: Channel, member: keyof Choice, value: string) =>
    setPrefs({
      ...prefs,
      [channel]: { ...prefs[channel], [member]: toggled(prefs[channel][member], value) },
    });

  const row = (label: string, member: keyof Choice, value: string, first: boolean) => (
    <tr key={`${member}-${value}`}>
      <th
        scope="row"
        className={`py-2 text-left font-normal text-fg ${first ? 'border-t border-line-strong' : 'border-t border-line'}`}
      >
        {label}
      </th>
      {channels.map((ch) => (
        <td
          key={ch}
          className={`text-center ${first ? 'border-t border-line-strong' : 'border-t border-line'}`}
        >
          <span className="inline-flex justify-center">
            <Checkbox
              aria-label={`${CHANNEL_LABEL[ch]}: ${label.toLowerCase()} alerts`}
              checked={prefs[ch][member].includes(value)}
              onChange={() => set(ch, member, value)}
            />
          </span>
        </td>
      ))}
    </tr>
  );

  return (
    <div className="flex flex-col gap-6">
      <Field label="Email digest" hint="A summary of alerts in your scope.">
        <Segmented<Digest>
          label="Email digest"
          value={prefs.digest}
          onChange={(digest) => setPrefs({ ...prefs, digest })}
          options={[
            { value: 'off', label: 'Off' },
            { value: 'daily', label: 'Daily' },
            { value: 'weekly', label: 'Weekly' },
          ]}
        />
      </Field>
      <div className="min-w-0 overflow-x-auto">
        <table className="w-full border-separate border-spacing-0 text-ui">
          <caption className="mb-2 text-left text-body font-semibold text-fg">
            Tell me about each alert
            <span className="block text-caption font-normal text-muted">
              An alert is sent when both its severity and its category are chosen.
            </span>
          </caption>
          <thead>
            <tr>
              <th scope="col" className="py-1.5 text-left text-caption font-medium text-muted">
                Severity and category
              </th>
              {channels.map((ch) => (
                <th key={ch} scope="col" className="w-20 py-1.5 text-caption font-medium text-muted">
                  {CHANNEL_LABEL[ch]}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {SEVERITIES.map((s, i) => row(SEVERITY_LABEL[s], 'severities', s, i === 0))}
            {CATEGORIES.map((c, i) => row(CATEGORY_LABEL[c], 'categories', c, i === 0))}
          </tbody>
        </table>
      </div>
      <p className="text-caption text-muted">
        Only alerts you can see are sent. On-call channels for the whole team are set up on the Alerts screen.
      </p>
      <div className="flex flex-col gap-3">
        <InlineError error={error} />
        <div>
          <Button variant="primary" busy={busy} onClick={() => void save(prefs)}>
            Save notification settings
          </Button>
        </div>
      </div>
      {pushOffered ? (
        <div className="border-t border-line pt-5">
          <BrowsersPanel push={push} />
        </div>
      ) : null}
    </div>
  );
}
