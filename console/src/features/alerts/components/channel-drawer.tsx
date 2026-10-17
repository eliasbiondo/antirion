'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, MultiSelect, Segmented, Select } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { MenuButton } from '@/components/ui/menu';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { formatCount, plural } from '@/lib/format';
import { useEdited, useNow } from '@/lib/hooks';
import type { FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { agoWords, CHANNEL_TYPES } from '../describe';
import { useAlertEditors } from '../editors';
import {
  CHANNEL_FIELD,
  channelErrors,
  channelForm,
  channelSpec,
  mapServerErrors,
  type ChannelForm,
} from '../form';
import { useAlerts } from '../store';
import { runTest } from './channels-view';

const SECRET_HINT = 'Stored encrypted and never shown again. A reference such as ${env:NAME} also works.';

/**
 * Creates or edits a notification channel: Slack, PagerDuty, email or an event webhook, with the
 * lowest severity it receives, its last delivery and a test send.
 */
export function ChannelDrawer({ name, webhook }: { name: string | null; webhook?: string | undefined }) {
  const close = useAlertEditors((s) => s.close);
  const show = useAlertEditors((s) => s.show);
  const channels = useAlerts((s) => s.channels.data);
  const status = useAlerts((s) => s.channelStatus.data);
  const rules = useAlerts((s) => s.rules.data);
  const hooks = useAlerts((s) => s.webhooks.data?.items);
  const save = useAlerts((s) => s.saveChannel);
  const remove = useAlerts((s) => s.deleteChannel);
  const testChannel = useAlerts((s) => s.testChannel);
  const existing = name ? (channels?.find((c) => c.name === name) ?? null) : null;
  const canWrite = useCan(existing ? 'replaceAlertChannel' : 'createAlertChannel');
  const canDelete = useCan('deleteAlertChannel');
  const canTest = useCan('testAlertChannel');
  const [form, setForm] = useState<ChannelForm>(() => channelForm(existing, webhook));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form);
  const [testing, setTesting] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const now = useNow(30_000);
  const set = <K extends keyof ChannelForm>(key: K, value: ChannelForm[K]) => {
    setForm((f) => ({ ...f, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  };
  const taken = (channels ?? []).map((c) => c.name);
  const hint = (path: string) => existing?.metadata.secret_hints?.[path]?.replace(/^…/, '');
  const sameType = existing?.spec.type === form.type;
  const st = status?.find((s) => s.name === name);
  const used = (rules ?? []).filter(
    (r) => name && ((r.spec.channels ?? []).includes(name) || (r.spec.escalation ?? []).includes(name)),
  );

  async function submit(): Promise<boolean> {
    const found = channelErrors(form, existing, taken);
    setErrors(found);
    if (Object.keys(found).length) return false;
    setBusy(true);
    setError(null);
    try {
      await save(form.name, channelSpec(form, existing), existing);
      notify('success', existing ? 'Channel saved' : 'Channel added', form.name);
      return true;
    } catch (e) {
      setError(e);
      setErrors(mapServerErrors<ChannelForm>(e, `observability.alerts.channels.${form.name}`, CHANNEL_FIELD));
      return false;
    } finally {
      setBusy(false);
    }
  }

  const secretField = (label: string, key: 'slackUrl' | 'routingKey', path: string, placeholder: string) => {
    const stored = sameType && hint(path) !== undefined;
    const kept = sameType && !!existing;
    return (
      <Field
        label={label}
        required={!kept}
        error={errors[key]}
        hint={
          kept
            ? `${stored ? `Stored, ending ${hint(path) ?? ''}. ` : 'Stored. '}Leave empty to keep it.`
            : SECRET_HINT
        }
      >
        <Input
          mono
          type="password"
          autoComplete="off"
          value={form[key]}
          placeholder={kept ? 'Unchanged' : placeholder}
          onChange={(e) => set(key, e.target.value)}
        />
      </Field>
    );
  };
  const hookNames = Object.keys(hooks ?? {}).sort();

  return (
    <Drawer
      dirty={edited}
      open
      onClose={close}
      title={existing ? existing.name : 'New notification channel'}
      subtitle={existing ? `${CHANNEL_TYPES[existing.spec.type]} channel` : 'Where alerts are sent.'}
      footer={
        <>
          {existing ? (
            <span className="mr-auto flex items-center gap-2">
              <Button
                icon="send"
                busy={testing}
                disabled={!canTest}
                disabledReason={canTest ? undefined : "You don't have permission to test channels."}
                onClick={() => {
                  setTesting(true);
                  void runTest(existing.name, testChannel).finally(() => setTesting(false));
                }}
              >
                Send test
              </Button>
              <MenuButton
                label="More"
                align="left"
                items={[
                  {
                    label: 'Delete channel',
                    icon: 'trash',
                    danger: true,
                    disabled: !canDelete,
                    disabledReason: canDelete ? undefined : "You don't have permission to delete channels.",
                    onSelect: () => setConfirm(true),
                  },
                ]}
              />
            </span>
          ) : null}
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!canWrite}
            disabledReason={canWrite ? undefined : "You don't have permission to change channels."}
            onClick={() => void submit().then((ok) => ok && close())}
          >
            {existing ? 'Save changes' : 'Add channel'}
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          void submit().then((ok) => ok && close());
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
              { label: 'Used by', value: used.length ? used.map((r) => r.name).join(', ') : 'No rule yet' },
            ]}
          />
        ) : null}
        {existing ? null : (
          <Field label="Name" error={errors.name} required hint="Rules name the channels they notify.">
            <Input
              mono
              value={form.name}
              placeholder="e.g. oncall-slack"
              onChange={(e) => set('name', e.target.value)}
              data-autofocus
            />
          </Field>
        )}
        <div className="flex flex-col gap-1.5">
          <span className="text-ui font-medium text-fg">Type</span>
          <Segmented<ChannelForm['type']>
            label="Channel type"
            value={form.type}
            onChange={(v) => set('type', v)}
            options={[
              { value: 'slack', label: 'Slack', icon: 'messages' },
              { value: 'pagerduty', label: 'PagerDuty', icon: 'phone' },
              { value: 'email', label: 'Email', icon: 'mail' },
              { value: 'webhook', label: 'Webhook', icon: 'webhook' },
            ]}
            className="w-fit max-sm:w-full"
          />
          {errors.type ? <p className="text-caption text-error">{errors.type}</p> : null}
        </div>
        {form.type === 'slack'
          ? secretField(
              'Incoming webhook URL',
              'slackUrl',
              'slack.webhook_url',
              'https://hooks.slack.com/services/…',
            )
          : null}
        {form.type === 'pagerduty' ? (
          <>
            {secretField(
              'Integration key',
              'routingKey',
              'pagerduty.routing_key',
              'The Events API v2 routing key',
            )}
            <Field
              label="Events API URL"
              optional
              error={errors.eventsUrl}
              hint="Only for a PagerDuty region or proxy other than the default."
            >
              <Input
                mono
                value={form.eventsUrl}
                placeholder="https://events.pagerduty.com/v2/enqueue"
                onChange={(e) => set('eventsUrl', e.target.value)}
              />
            </Field>
          </>
        ) : null}
        {form.type === 'email' ? (
          <Field
            label="Send to"
            required
            error={errors.emails}
            hint="Sent through the gateway's outgoing mail, which must be set up in Settings."
          >
            <MultiSelect
              mono
              creatable
              noun="address"
              placeholder="Type an address and press Enter"
              options={form.emails.map((e) => ({ value: e, label: e }))}
              value={form.emails}
              onChange={(v) => set('emails', v)}
            />
          </Field>
        ) : null}
        {form.type === 'webhook' ? (
          <Field
            label="Event webhook"
            required
            error={errors.webhook}
            hint="Alerts are posted to it as alert.fired, alert.acknowledged and alert.resolved events."
            aside={
              <button
                type="button"
                className="text-caption text-accent hover:underline"
                onClick={() => show({ kind: 'webhook', name: null, events: ['alert.*'] }, { over: true })}
              >
                New webhook
              </button>
            }
          >
            <Select
              mono
              value={form.webhook}
              placeholder={hookNames.length ? 'Choose a webhook' : 'No event webhooks yet'}
              onChange={(v) => set('webhook', v)}
              options={hookNames.map((n) => ({ value: n, label: n, description: hooks?.[n]?.url }))}
            />
          </Field>
        ) : null}
        <Field label="Sends" hint="The lowest severity this channel receives.">
          <Select
            value={form.minSeverity}
            onChange={(v) => set('minSeverity', v as ChannelForm['minSeverity'])}
            options={[
              { value: 'info', label: 'All alerts' },
              { value: 'warning', label: 'Warning and above' },
              { value: 'critical', label: 'Critical only' },
            ]}
          />
        </Field>
        <Switch checked={form.enabled} onChange={(v) => set('enabled', v)} label="Channel is on" />
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
      {existing ? (
        <ConfirmDialog
          open={confirm}
          onClose={() => setConfirm(false)}
          title={`Delete channel ${existing.name}?`}
          typeToConfirm={existing.name}
          confirmLabel="Delete channel"
          objectName={existing.name}
          effect={
            used.length
              ? `${plural(used.length, 'rule')} still notify it (${used.map((r) => r.name).join(', ')}); remove it from them first, or the gateway refuses the change.`
              : 'Alerts stop going to this destination.'
          }
          onConfirm={async () => {
            await remove(existing);
            notify('success', 'Channel deleted', existing.name);
            close();
          }}
        />
      ) : null}
    </Drawer>
  );
}
