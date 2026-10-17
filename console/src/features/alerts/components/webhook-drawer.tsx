'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Checkbox, Switch } from '@/components/ui/choice';
import { Field, Input, Select } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { InlineError } from '@/components/ui/states';
import { describeError } from '@/lib/api/client';
import { formatCount } from '@/lib/format';
import { useEdited, useNow } from '@/lib/hooks';
import type { FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { alertsApi } from '../api';
import { agoWords } from '../describe';
import { useAlertEditors } from '../editors';
import { covers, EVENT_SOURCES, eventWords } from '../events';
import { mapServerErrors, webhookErrors, webhookForm, type WebhookForm } from '../form';
import { useAlerts } from '../store';
import { runTest } from './channels-view';

const TIMEOUTS = ['2s', '5s', '10s', '30s'];

/** The subscription as boxes: a whole source (`budget.*`) or its events one by one, plus everything. */
function EventPicker({
  value,
  onChange,
  error,
}: {
  value: string[];
  onChange: (v: string[]) => void;
  error?: string;
}) {
  const all = value.includes('*');
  const toggle = (pattern: string, on: boolean) =>
    onChange(on ? [...value, pattern] : value.filter((p) => p !== pattern));
  return (
    <fieldset className="flex flex-col gap-2" aria-describedby={error ? 'webhook-events-error' : undefined}>
      <legend className="mb-1 text-ui font-medium text-fg">
        Events{' '}
        <span className="text-error" aria-hidden="true">
          *
        </span>
      </legend>
      {error ? (
        <p id="webhook-events-error" role="alert" className="text-caption text-error">
          {error}
        </p>
      ) : null}
      <Checkbox
        checked={all}
        onChange={(on) => onChange(on ? ['*'] : [])}
        label="Every event"
        description="Including event types added by later gateway versions."
      />
      {all ? null : (
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 max-sm:grid-cols-1">
          {EVENT_SOURCES.map((source) => {
            const prefix = `${source.prefix}.*`;
            const single = source.events.length === 1 ? source.events[0] : undefined;
            if (single) {
              const on = value.includes(single.type) || value.includes(prefix);
              return (
                <div key={source.prefix} className="rounded-control border border-line px-3 py-2">
                  <Checkbox
                    checked={on}
                    onChange={(next) =>
                      onChange([
                        ...value.filter((p) => p !== prefix && p !== single.type),
                        ...(next ? [single.type] : []),
                      ])
                    }
                    label={source.label}
                    description={single.label}
                  />
                </div>
              );
            }
            const whole = value.includes(prefix);
            const some = source.events.filter((e) => value.includes(e.type)).length;
            return (
              <div
                key={source.prefix}
                className="flex flex-col gap-1 rounded-control border border-line px-3 py-2"
              >
                <Checkbox
                  checked={whole ? true : some ? 'mixed' : false}
                  onChange={(on) =>
                    onChange([
                      ...value.filter((p) => p !== prefix && !source.events.some((e) => e.type === p)),
                      ...(on ? [prefix] : []),
                    ])
                  }
                  label={`${source.label}, every event`}
                />
                {whole ? null : (
                  <div className="ml-6 flex flex-col gap-1">
                    {source.events.map((e) => (
                      <Checkbox
                        key={e.type}
                        checked={value.includes(e.type)}
                        onChange={(on) => toggle(e.type, on)}
                        label={<span className="text-ui">{e.label}</span>}
                      />
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
      {value.filter(
        (p) =>
          p !== '*' &&
          !EVENT_SOURCES.some((s) => p === `${s.prefix}.*` || s.events.some((e) => e.type === p)),
      ).length ? (
        <p className="text-caption text-muted">
          Also subscribed to{' '}
          {value
            .filter(
              (p) =>
                p !== '*' &&
                !EVENT_SOURCES.some((s) => p === `${s.prefix}.*` || s.events.some((e) => e.type === p)),
            )
            .join(', ')}
          .
        </p>
      ) : null}
    </fieldset>
  );
}

/** Creates or edits an event webhook: its endpoint, the events it subscribes to, pause, test and signing secret. */
export function WebhookDrawer({ name, events }: { name: string | null; events?: string[] | undefined }) {
  const close = useAlertEditors((s) => s.close);
  const webhooks = useAlerts((s) => s.webhooks.data);
  const status = useAlerts((s) => s.webhookStatus.data);
  const save = useAlerts((s) => s.saveWebhook);
  const testWebhook = useAlerts((s) => s.testWebhook);
  const existing = name ? webhooks?.items[name] : undefined;
  const canWrite = useCan('patchObservabilitySettings');
  const canTest = useCan('testWebhook');
  const canRotate = useCan('rotateWebhookSecret');
  const [form, setForm] = useState<WebhookForm>(() => webhookForm(name, existing, events));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form);
  const [testing, setTesting] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  const now = useNow(30_000);
  const set = <K extends keyof WebhookForm>(key: K, value: WebhookForm[K]) => {
    setForm((f) => ({ ...f, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  };
  const st = status?.find((s) => s.name === name);
  const covered = EVENT_SOURCES.flatMap((s) => s.events).filter((e) =>
    form.events.some((p) => covers(p, e.type)),
  );

  async function submit() {
    const found = webhookErrors(form, !existing, Object.keys(webhooks?.items ?? {}));
    setErrors(found);
    if (Object.keys(found).length) return;
    setBusy(true);
    setError(null);
    try {
      await save(form.name, {
        url: form.url.trim(),
        events: form.events,
        enabled: form.enabled,
        timeout: form.timeout,
      });
      notify('success', existing ? 'Webhook saved' : 'Webhook added', form.name);
      close();
    } catch (e) {
      setError(e);
      setErrors(
        mapServerErrors<WebhookForm>(e, `observability.alerts.webhooks.${form.name}`, {
          url: 'url',
          events: 'events',
          timeout: 'timeout',
        }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <Drawer
      dirty={edited}
      open
      onClose={close}
      width="lg"
      title={existing ? (name ?? '') : 'New event webhook'}
      subtitle="Each event is posted as JSON, signed when the webhook has a signing secret, and retried for an hour."
      footer={
        <>
          {existing && name ? (
            <span className="mr-auto flex items-center gap-2">
              <Button
                icon="send"
                busy={testing}
                disabled={!canTest}
                disabledReason={canTest ? undefined : "You don't have permission to test webhooks."}
                onClick={() => {
                  setTesting(true);
                  void runTest(name, testWebhook).finally(() => setTesting(false));
                }}
              >
                Send test
              </Button>
              <Button
                variant="ghost"
                icon="key"
                disabled={!canRotate}
                disabledReason={canRotate ? undefined : "You don't have permission to generate secrets."}
                onClick={() =>
                  void alertsApi.rotateWebhookSecret(name).then(
                    (r) => setSecret(r.secret),
                    (e: unknown) => notify('error', "Couldn't generate a secret", describeError(e)),
                  )
                }
              >
                {existing.signing_secret ? 'New signing secret' : 'Add a signing secret'}
              </Button>
            </span>
          ) : null}
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!canWrite}
            disabledReason={canWrite ? undefined : "You don't have permission to change webhooks."}
            onClick={() => void submit()}
          >
            {existing ? 'Save changes' : 'Add webhook'}
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <InlineError error={error} />
        {existing ? (
          <DefinitionGrid
            columns={3}
            items={[
              {
                label: 'Last delivery',
                value: st?.last_delivery_at
                  ? `${st.last_status && st.last_status < 300 ? 'Delivered' : 'Failed'} ${agoWords(st.last_delivery_at, now)}`
                  : 'Never',
              },
              {
                label: 'Last 24 h',
                value: st
                  ? `${formatCount(st.deliveries_24h)} sent${st.failures_24h ? `, ${formatCount(st.failures_24h)} failed` : ''}`
                  : '—',
              },
              { label: 'Queued', value: st ? formatCount(st.queued) : '—' },
            ]}
          />
        ) : (
          <Field
            label="Name"
            required
            error={errors.name}
            hint="Channels of type webhook refer to it by this name."
          >
            <Input
              mono
              value={form.name}
              placeholder="e.g. ops-events"
              onChange={(e) => set('name', e.target.value)}
              data-autofocus
            />
          </Field>
        )}
        <Field label="Endpoint URL" required error={errors.url}>
          <Input
            mono
            value={form.url}
            placeholder="https://hooks.example.com/antirion"
            onChange={(e) => set('url', e.target.value)}
          />
        </Field>
        <EventPicker value={form.events} onChange={(v) => set('events', v)} error={errors.events} />
        <p className="text-caption text-muted" aria-live="polite">
          {form.events.length === 0
            ? 'Nothing subscribed yet.'
            : form.events.includes('*')
              ? 'Receives every event.'
              : `Receives ${covered.length} event types: ${form.events.map(eventWords).join(', ')}.`}
        </p>
        <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
          <Field label="Timeout" hint="An answer slower than this counts as a failed attempt.">
            <Select
              value={form.timeout}
              onChange={(v) => set('timeout', v)}
              options={(TIMEOUTS.includes(form.timeout) ? TIMEOUTS : [...TIMEOUTS, form.timeout]).map(
                (t) => ({
                  value: t,
                  label: t.replace('s', ' s'),
                }),
              )}
            />
          </Field>
        </div>
        <div className="flex flex-col gap-1">
          <Switch checked={form.enabled} onChange={(v) => set('enabled', v)} label="Deliver events" />
          <p className="text-caption text-muted">
            A paused webhook queues its events and sends them when it is resumed, for up to an hour.
          </p>
        </div>
        {existing ? null : (
          <p className="text-caption text-muted">
            After adding it, generate a signing secret so the endpoint can check each delivery came from the
            gateway.
          </p>
        )}
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
      <RevealOnceDialog
        secret={secret}
        onClose={() => setSecret(null)}
        title={`Signing secret for ${name ?? ''}`}
        label="Signing secret"
        description="Verify each delivery's x-antirion-signature header with it. It is shown only now; the previous secret keeps working for an hour."
      />
    </Drawer>
  );
}
