'use client';

import { useState } from 'react';

import { Button, buttonClass } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { FavouriteButton } from '@/components/ui/favourite';
import { Field, Input, Select, Switch } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { CopyButton, CopyLinkButton } from '@/components/ui/misc';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { serverFieldErrors, type FieldErrors } from '@/lib/validation';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import { dataPlaneAddress } from '@/features/mcp/fields';
import { Facts, FormGroup, HeaderRowsEditor, ListInput, Section } from '@/features/mcp/components/parts';

import type { AgentResource } from '../api';
import {
  agentErrors,
  agentForm,
  type AgentForm,
  agentSpec,
  EMPTY_AGENT,
  type Protocol,
  PROTOCOL_LABEL,
} from '../form';
import { useAgents } from '../store';
import { useEdited } from '@/lib/hooks';

const NONE: never[] = [];

function AgentView({ agent }: { agent: AgentResource }) {
  const base = useShell((s) => s.status?.data_plane_url ?? null);
  const s = agent.spec;
  const address = dataPlaneAddress(base, `/a2a/${agent.name}`);
  const headers = Object.keys(s.headers ?? {});
  const identity = Object.entries(s.identity_headers ?? {});
  return (
    <div className="flex flex-col gap-6">
      <Section title="Address" aside="Clients call the agent here">
        <div className="flex items-center gap-2">
          <span className="mono min-w-0 flex-1 truncate rounded-control border border-line bg-sunken px-3 py-1.5 text-ui text-fg">
            {address}
          </span>
          <CopyButton value={address} label={`Copy ${address}`} />
        </div>
        <p className="mt-1.5 text-caption text-muted">
          Its card is at <span className="mono">{address}/.well-known/agent-card.json</span>
          {s.public_card ? ', readable without a key.' : ', for callers with an API key.'}
          {!base ? ' Set the gateway URL in Settings to show the full address.' : ''}
        </p>
      </Section>
      <Section title="Settings">
        <Facts
          items={[
            {
              label: 'Agent card',
              value: (
                <a
                  href={s.card_url}
                  target="_blank"
                  rel="noreferrer noopener"
                  className="text-accent hover:underline"
                >
                  {s.card_url}
                </a>
              ),
            },
            { label: 'Calls go to', mono: Boolean(s.url), value: s.url ?? 'The address in its card' },
            { label: 'Protocol', mono: false, value: PROTOCOL_LABEL[s.protocol ?? 'auto'] },
            { label: 'Card', mono: false, value: s.public_card ? 'Public' : 'Needs an API key' },
            {
              label: 'Signs in with',
              mono: false,
              value: headers.length > 0 ? `Headers: ${headers.join(', ')} (values hidden)` : 'Nothing',
            },
            ...(identity.length > 0
              ? [{ label: 'Identity headers', value: identity.map(([h, v]) => `${h}: ${v}`).join(', ') }]
              : []),
            {
              label: 'Timeouts',
              mono: false,
              value:
                [
                  s.request_timeout ? `request ${s.request_timeout}` : null,
                  s.stream_idle_timeout ? `idle stream ${s.stream_idle_timeout}` : null,
                  s.card_cache_ttl ? `card kept ${s.card_cache_ttl}` : null,
                ]
                  .filter(Boolean)
                  .join(' · ') || 'Defaults',
            },
            ...(s.extra_methods?.length
              ? [{ label: 'Extra methods', value: s.extra_methods.join(', ') }]
              : []),
          ]}
        />
      </Section>
      <p className="text-caption text-muted">
        Calls to this agent appear in the request log, and priced calls in the Budgets ledger.
      </p>
    </div>
  );
}

/** One A2A agent: where clients call it and its settings, or the form that adds or edits it. */
export function AgentDrawer({
  agent,
  creating,
  onClose,
  onCreated,
}: {
  agent: AgentResource | null;
  creating: boolean;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const agents = useAgents((s) => s.agents.data) ?? NONE;
  const save = useAgents((s) => s.save);
  const remove = useAgents((s) => s.remove);
  const canEdit = useCan('replaceA2aAgent');
  const canDelete = useCan('deleteA2aAgent');
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<AgentForm>(() =>
    agent && !creating ? agentForm(agent) : EMPTY_AGENT,
  );
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const edited = useEdited(values, creating || editing);
  const form = creating || editing;
  const name = agent?.name ?? null;

  const set = <K extends keyof AgentForm>(key: K, value: AgentForm[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  };

  async function submit() {
    const found = agentErrors(
      values,
      agents.map((a) => a.name),
      creating,
    );
    if (Object.keys(found).length > 0) {
      setErrors(found);
      return;
    }
    const target = creating ? values.name.trim() : (agent?.name ?? '');
    setBusy(true);
    setError(null);
    try {
      await save(target, agentSpec(values, creating ? null : agent), creating ? null : agent);
      notify('success', creating ? 'Agent added' : 'Agent saved', target);
      setEditing(false);
      if (creating) onCreated(target);
    } catch (e) {
      setError(e);
      const fields = serverFieldErrors(e, `a2a.agents.${target}`);
      setErrors({
        ...(fields.card_url ? { cardUrl: fields.card_url } : {}),
        ...(fields.url ? { url: fields.url } : {}),
        ...(fields.headers ? { headers: fields.headers } : {}),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Drawer
        dirty={(creating || editing) && edited}
        open={creating || agent !== null}
        onClose={onClose}
        width="lg"
        title={creating ? 'Add agent' : (agent?.name ?? 'Agent')}
        subtitle={
          creating
            ? 'An A2A agent that apps and other agents call through the gateway, with its card.'
            : agent
              ? `${PROTOCOL_LABEL[agent.spec.protocol ?? 'auto']}${agent.spec.enabled === false ? ' · off' : ''}`
              : undefined
        }
        headerActions={
          agent && !creating ? (
            <>
              <a
                href={agent.spec.card_url}
                target="_blank"
                rel="noreferrer noopener"
                className={buttonClass('secondary', 'sm')}
              >
                <Icon name="external" size={14} />
                Agent card
              </a>
              <FavouriteButton kind="agent" name={agent.name} label={`agent ${agent.name}`} />
              <CopyLinkButton />
            </>
          ) : undefined
        }
        footer={
          form ? (
            <>
              {creating ? (
                <CancelButton />
              ) : (
                <Button
                  variant="ghost"
                  onClick={() => {
                    setEditing(false);
                    if (agent) setValues(agentForm(agent));
                  }}
                >
                  Cancel
                </Button>
              )}
              <Button variant="primary" busy={busy} onClick={() => void submit()}>
                {creating ? 'Add agent' : 'Save changes'}
              </Button>
            </>
          ) : agent && (canEdit || canDelete) ? (
            <>
              {canDelete ? (
                <Button variant="danger" icon="trash" onClick={() => setDeleting(true)} className="mr-auto">
                  Delete
                </Button>
              ) : null}
              {canEdit ? (
                <Button variant="primary" icon="edit" onClick={() => setEditing(true)}>
                  Edit
                </Button>
              ) : null}
            </>
          ) : undefined
        }
      >
        {form ? (
          <form
            noValidate
            className="flex flex-col gap-5"
            onSubmit={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            <InlineError error={error} />
            {creating ? (
              <Field
                label="Name"
                required
                error={errors.name}
                hint="Clients call it at /a2a/<name>. It can't be changed later."
              >
                <Input
                  mono
                  autoComplete="off"
                  value={values.name}
                  onChange={(e) => set('name', e.target.value)}
                  data-autofocus
                />
              </Field>
            ) : null}
            <Field
              label="Agent card URL"
              required
              error={errors.cardUrl}
              hint="Usually ends in /.well-known/agent-card.json."
            >
              <Input
                mono
                inputMode="url"
                value={values.cardUrl}
                onChange={(e) => set('cardUrl', e.target.value)}
              />
            </Field>
            <Field
              label="Send calls to"
              optional
              error={errors.url}
              hint="Leave empty to use the address in the agent's card."
            >
              <Input mono inputMode="url" value={values.url} onChange={(e) => set('url', e.target.value)} />
            </Field>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Protocol">
                <Select
                  value={values.protocol}
                  onChange={(v) => set('protocol', v as Protocol)}
                  options={(Object.keys(PROTOCOL_LABEL) as Protocol[]).map((p) => ({
                    value: p,
                    label: PROTOCOL_LABEL[p],
                  }))}
                />
              </Field>
            </div>
            <Switch label="On" checked={values.enabled} onChange={(v) => set('enabled', v)} />
            <Switch
              label="Public card: anyone may read the agent's card; calls still need an API key"
              checked={values.publicCard}
              onChange={(v) => set('publicCard', v)}
            />
            <FormGroup title="Signing in to the agent">
              <HeaderRowsEditor
                label="Headers sent to the agent"
                rows={values.headers}
                onChange={(rows) => set('headers', rows)}
                error={errors.headers}
                hint="Sent only to this agent. Values are stored encrypted and never shown again."
              />
            </FormGroup>
            <FormGroup title="Timeouts and methods" hint="Leave empty for the gateway's defaults.">
              <div className="grid gap-4 sm:grid-cols-3">
                <Field label="Request" optional error={errors.requestTimeout} hint="Default 300s">
                  <Input
                    mono
                    value={values.requestTimeout}
                    onChange={(e) => set('requestTimeout', e.target.value)}
                  />
                </Field>
                <Field label="Idle stream" optional error={errors.streamIdleTimeout} hint="Default 300s">
                  <Input
                    mono
                    value={values.streamIdleTimeout}
                    onChange={(e) => set('streamIdleTimeout', e.target.value)}
                  />
                </Field>
                <Field label="Keep the card" optional error={errors.cardCacheTtl} hint="Default 5m">
                  <Input
                    mono
                    value={values.cardCacheTtl}
                    onChange={(e) => set('cardCacheTtl', e.target.value)}
                  />
                </Field>
              </div>
              <Field
                label="Extra methods"
                optional
                hint="JSON-RPC methods beyond the standard ones to pass on."
              >
                <ListInput
                  noun="method"
                  value={values.extraMethods}
                  onChange={(v) => set('extraMethods', v)}
                />
              </Field>
            </FormGroup>
            <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
          </form>
        ) : agent ? (
          <AgentView agent={agent} />
        ) : null}
      </Drawer>
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title={`Delete agent ${name ?? ''}?`}
        objectName={name ?? undefined}
        typeToConfirm={name ?? undefined}
        confirmLabel="Delete agent"
        effect="Clients calling it through the gateway get “not found” at once. The agent itself is not changed."
        onConfirm={async (reason) => {
          if (!name) return;
          await remove(name, reason);
          notify('success', 'Agent deleted', name);
          onClose();
        }}
      />
    </>
  );
}
