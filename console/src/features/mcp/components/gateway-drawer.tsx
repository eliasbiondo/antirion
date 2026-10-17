'use client';

import { useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Select, Switch } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CopyButton, CopyLinkButton } from '@/components/ui/misc';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { EmptyPill, InlineError } from '@/components/ui/states';
import { formatCount } from '@/lib/format';
import { serverFieldErrors, type FieldErrors } from '@/lib/validation';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import type { McpGatewayResource } from '../api';
import { dataPlaneAddress } from '../fields';
import {
  EMPTY_GATEWAY,
  gatewayErrors,
  gatewayForm,
  type GatewayForm,
  gatewayPaths,
  gatewaySpec,
} from '../form';
import { useMcp } from '../store';
import { SESSION_MODE_LABEL } from './gateways-tab';
import { Facts, FormGroup, ListInput, Section } from './parts';
import { useEdited } from '@/lib/hooks';

const NONE: never[] = [];

const PREFIX_LABEL: Record<GatewayForm['prefixMode'], string> = {
  auto: 'When it combines more than one server',
  always: 'Always',
  never: 'Never',
};

function ServerOrder({
  value,
  options,
  onChange,
  error,
}: {
  value: string[];
  options: string[];
  onChange: (v: string[]) => void;
  error?: string | undefined;
}) {
  const move = (i: number, by: number) => {
    const next = [...value];
    const [item] = next.splice(i, 1);
    if (item !== undefined) next.splice(i + by, 0, item);
    onChange(next);
  };
  const free = options.filter((o) => !value.includes(o));
  return (
    <div role="group" aria-label="Servers" className="flex flex-col gap-2">
      <span className="text-ui font-medium text-fg">
        Servers <span className="font-normal text-muted">in order: the first wins a name both have</span>
      </span>
      <ol className="flex flex-col border-y border-line">
        {value.length === 0 ? (
          <li className="flex justify-center px-3 py-3">
            <EmptyPill>No server yet</EmptyPill>
          </li>
        ) : null}
        {value.map((s, i) => (
          <li key={s} className="flex items-center gap-2 border-line px-3 py-1.5 [&+&]:border-t">
            <span className="mono w-5 text-caption text-dim tabular-nums">{i + 1}</span>
            <span className="mono flex-1 text-ui text-fg">{s}</span>
            <IconButton
              icon="arrowUp"
              label={`Move ${s} up`}
              disabled={i === 0}
              onClick={() => move(i, -1)}
            />
            <IconButton
              icon="arrowDown"
              label={`Move ${s} down`}
              disabled={i === value.length - 1}
              onClick={() => move(i, 1)}
            />
            <IconButton
              icon="x"
              label={`Remove ${s}`}
              onClick={() => onChange(value.filter((x) => x !== s))}
            />
          </li>
        ))}
      </ol>
      {free.length > 0 ? (
        <div className="w-64 max-sm:w-full">
          <Select
            aria-label="Add a server"
            value=""
            placeholder="Add a server"
            onChange={(v) => v && onChange([...value, v])}
            options={free.map((s) => ({ value: s, label: s }))}
          />
        </div>
      ) : null}
      {error ? (
        <p role="alert" className="text-caption text-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}

function GatewayView({ gateway }: { gateway: McpGatewayResource }) {
  const base = useShell((s) => s.status?.data_plane_url ?? null);
  const stats = useMcp((s) => s.sessions.data?.find((x) => x.gateway === gateway.name));
  const s = gateway.spec;
  const info = s.server_info;
  return (
    <div className="flex flex-col gap-6">
      <Section title="Address" aside="Clients connect here with an API key">
        <ul className="flex flex-col gap-1.5">
          {gatewayPaths(gateway.name).map((p) => {
            const url = dataPlaneAddress(base, p);
            return (
              <li key={p} className="flex items-center gap-2">
                <span className="mono min-w-0 flex-1 truncate rounded-control border border-line bg-sunken px-3 py-1.5 text-ui text-fg">
                  {url}
                </span>
                <CopyButton value={url} label={`Copy ${url}`} />
              </li>
            );
          })}
        </ul>
        {!base ? (
          <p className="mt-1.5 text-caption text-muted">
            Set the gateway URL in Settings to show the full address.
          </p>
        ) : null}
      </Section>
      <Section title="Servers" aside="In order">
        <ol className="flex flex-wrap gap-1.5">
          {s.servers.map((name, i) => (
            <li key={name}>
              <Badge mono>
                {i + 1}. {name}
              </Badge>
            </li>
          ))}
        </ol>
      </Section>
      <Section title="Settings">
        <Facts
          items={[
            { label: 'Sessions', mono: false, value: SESSION_MODE_LABEL[s.session_mode ?? 'stateful'] },
            { label: 'Prefix tool names', mono: false, value: PREFIX_LABEL[s.prefix_mode ?? 'auto'] },
            {
              label: 'When a server fails',
              mono: false,
              value:
                s.on_server_failure === 'fail'
                  ? 'Fail the whole request'
                  : "Answer with the other servers' tools",
            },
            {
              label: 'Tool search',
              mono: false,
              value: s.tool_search
                ? 'On: clients see three search tools and find the others through them'
                : 'Off: clients see every tool',
            },
            {
              label: 'Server name shown',
              mono: false,
              value: info === 'upstream' ? "The server's own" : info?.name ? info.name : 'Antirion',
            },
            ...(s.allowed_origins?.length
              ? [{ label: 'Browser origins', value: s.allowed_origins.join(', ') }]
              : []),
            ...(s.max_body ? [{ label: 'Body limit', value: s.max_body }] : []),
            {
              label: 'OAuth',
              mono: false,
              value: s.oauth
                ? `Tokens from ${s.oauth.authorization_servers.join(', ')}`
                : 'Off: API keys only',
            },
          ]}
        />
      </Section>
      {stats ? (
        <Section title="Sessions" aside="This node, since it started">
          <Facts
            items={[
              { label: 'Issued', value: formatCount(stats.issued) },
              { label: 'Refused', value: formatCount(stats.rejected) },
              { label: 'Ended early', value: formatCount(stats.revoked) },
            ]}
          />
        </Section>
      ) : null}
    </div>
  );
}

/**
 * One MCP gateway: its address, servers and settings, or the form that adds or edits it. Keyed by
 * the caller on the gateway it shows.
 */
export function GatewayDrawer({
  gateway,
  creating,
  onClose,
  onCreated,
}: {
  gateway: McpGatewayResource | null;
  creating: boolean;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const gateways = useMcp((s) => s.gateways.data) ?? NONE;
  const servers = useMcp((s) => s.servers.data) ?? NONE;
  const save = useMcp((s) => s.saveGateway);
  const remove = useMcp((s) => s.removeGateway);
  const canEdit = useCan('replaceMcpGateway');
  const canDelete = useCan('deleteMcpGateway');
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<GatewayForm>(() =>
    gateway && !creating ? gatewayForm(gateway) : EMPTY_GATEWAY,
  );
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const edited = useEdited(values, creating || editing);
  const open = creating || gateway !== null;
  const form = creating || editing;
  const name = gateway?.name ?? null;

  const set = <K extends keyof GatewayForm>(key: K, value: GatewayForm[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  };

  async function submit() {
    const found = gatewayErrors(
      values,
      gateways.map((g) => g.name),
      creating,
    );
    if (Object.keys(found).length > 0) {
      setErrors(found);
      return;
    }
    const target = creating ? values.name.trim() : (gateway?.name ?? '');
    setBusy(true);
    setError(null);
    try {
      await save(target, gatewaySpec(values, creating ? null : gateway), creating ? null : gateway);
      notify('success', creating ? 'MCP gateway added' : 'MCP gateway saved', target);
      setEditing(false);
      if (creating) onCreated(target);
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `mcp.gateways.${target}`));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Drawer
        dirty={(creating || editing) && edited}
        open={open}
        onClose={onClose}
        width="lg"
        title={creating ? 'Add MCP gateway' : (gateway?.name ?? 'MCP gateway')}
        subtitle={
          creating
            ? 'The address clients connect to, combining one or more MCP servers.'
            : gateway
              ? `${gateway.spec.servers.length === 1 ? '1 server' : `${gateway.spec.servers.length} servers`}${gateway.spec.enabled === false ? ' · off' : ''}`
              : undefined
        }
        headerActions={gateway && !creating ? <CopyLinkButton /> : undefined}
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
                    if (gateway) setValues(gatewayForm(gateway));
                  }}
                >
                  Cancel
                </Button>
              )}
              <Button variant="primary" busy={busy} onClick={() => void submit()}>
                {creating ? 'Add gateway' : 'Save changes'}
              </Button>
            </>
          ) : gateway && (canEdit || canDelete) ? (
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
                hint="Clients connect at /mcp/<name>; a gateway named default also answers at /mcp."
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
            <ServerOrder
              value={values.servers}
              options={servers.map((s) => s.name)}
              onChange={(v) => set('servers', v)}
              error={errors.servers}
            />
            <Switch label="On" checked={values.enabled} onChange={(v) => set('enabled', v)} />
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Sessions">
                <Select
                  value={values.sessionMode}
                  onChange={(v) => set('sessionMode', v as GatewayForm['sessionMode'])}
                  options={[
                    { value: 'stateful', label: SESSION_MODE_LABEL.stateful },
                    { value: 'stateless', label: SESSION_MODE_LABEL.stateless },
                  ]}
                />
              </Field>
              <Field label="Prefix tool names with the server">
                <Select
                  value={values.prefixMode}
                  onChange={(v) => set('prefixMode', v as GatewayForm['prefixMode'])}
                  options={(Object.keys(PREFIX_LABEL) as GatewayForm['prefixMode'][]).map((k) => ({
                    value: k,
                    label: PREFIX_LABEL[k],
                  }))}
                />
              </Field>
              <Field label="When a server fails">
                <Select
                  value={values.onServerFailure}
                  onChange={(v) => set('onServerFailure', v as GatewayForm['onServerFailure'])}
                  options={[
                    { value: 'partial', label: "Answer with the other servers' tools" },
                    { value: 'fail', label: 'Fail the whole request' },
                  ]}
                />
              </Field>
              <Field label="Server name shown to clients" error={errors.serverInfo}>
                <Select
                  value={values.serverInfo}
                  onChange={(v) => set('serverInfo', v as GatewayForm['serverInfo'])}
                  options={[
                    { value: 'gateway', label: 'Antirion' },
                    { value: 'upstream', label: "The server's own (one server only)" },
                    { value: 'custom', label: 'Custom' },
                  ]}
                />
              </Field>
            </div>
            {values.serverInfo === 'custom' ? (
              <div className="grid gap-4 sm:grid-cols-3">
                <Field label="Name" optional>
                  <Input value={values.infoName} onChange={(e) => set('infoName', e.target.value)} />
                </Field>
                <Field label="Version" optional>
                  <Input
                    mono
                    value={values.infoVersion}
                    onChange={(e) => set('infoVersion', e.target.value)}
                  />
                </Field>
                <Field label="Title" optional>
                  <Input value={values.infoTitle} onChange={(e) => set('infoTitle', e.target.value)} />
                </Field>
              </div>
            ) : null}
            <Switch
              label="Tool search: list three search tools instead of every tool, for clients with many tools"
              checked={values.toolSearch}
              onChange={(v) => set('toolSearch', v)}
            />
            <FormGroup title="Browsers and limits">
              <Field
                label="Browser origins allowed"
                optional
                error={errors.allowedOrigins}
                hint="Such as https://app.example.com"
              >
                <ListInput
                  noun="origin"
                  value={values.allowedOrigins}
                  onChange={(v) => set('allowedOrigins', v)}
                />
              </Field>
              <Field label="Body limit" optional error={errors.maxBody} hint="Default 8MiB">
                <Input mono value={values.maxBody} onChange={(e) => set('maxBody', e.target.value)} />
              </Field>
            </FormGroup>
            <FormGroup
              title="OAuth"
              hint="Lets clients connect with tokens from your identity provider as well as API keys."
            >
              <Switch label="Accept OAuth tokens" checked={values.oauth} onChange={(v) => set('oauth', v)} />
              {values.oauth ? (
                <>
                  <Field label="Authorization servers" required error={errors.authorizationServers}>
                    <ListInput
                      noun="server"
                      value={values.authorizationServers}
                      onChange={(v) => set('authorizationServers', v)}
                    />
                  </Field>
                  <Field
                    label="Resource URL"
                    optional
                    hint="Defaults to every address the gateway is served at."
                  >
                    <Input
                      mono
                      value={values.oauthResource}
                      onChange={(e) => set('oauthResource', e.target.value)}
                    />
                  </Field>
                  <div className="grid gap-4 sm:grid-cols-2">
                    <Field label="Scopes" optional>
                      <ListInput noun="scope" value={values.scopes} onChange={(v) => set('scopes', v)} />
                    </Field>
                    <Field label="Audiences accepted" optional>
                      <ListInput
                        noun="audience"
                        value={values.audiences}
                        onChange={(v) => set('audiences', v)}
                      />
                    </Field>
                  </div>
                  <Switch
                    label="Relay client registration to the first authorization server"
                    checked={values.proxyRegistration}
                    onChange={(v) => set('proxyRegistration', v)}
                  />
                </>
              ) : null}
            </FormGroup>
            <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
          </form>
        ) : gateway ? (
          <GatewayView gateway={gateway} />
        ) : null}
      </Drawer>
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title={`Delete MCP gateway ${name ?? ''}?`}
        objectName={name ?? undefined}
        typeToConfirm={name ?? undefined}
        confirmLabel="Delete gateway"
        effect="Clients connected to it lose their sessions and get “not found”. The servers stay."
        onConfirm={async (reason) => {
          if (!name) return;
          await remove(name, reason);
          notify('success', 'MCP gateway deleted', name);
          onClose();
        }}
      />
    </>
  );
}
