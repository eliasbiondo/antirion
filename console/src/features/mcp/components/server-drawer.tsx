'use client';

import { useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { FavouriteButton } from '@/components/ui/favourite';
import { Field, Input, Segmented, Select, Switch } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CopyButton, CopyLinkButton } from '@/components/ui/misc';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { EmptyState, InlineError } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { ApiError } from '@/lib/api/client';
import { capitalize, plural, shortId } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { serverFieldErrors, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import type { McpServerResource } from '../api';
import {
  type AuthType,
  EMPTY_SERVER,
  HEALTH_LABEL,
  HEALTH_TONE,
  serverErrors,
  serverForm,
  type ServerForm,
  serverHealth,
  serverSpec,
  toolState,
  TRANSPORT_LABEL,
  type Transport,
  waitingTools,
} from '../form';
import { useMcp } from '../store';
import { Callout, Facts, FormGroup, HeaderRowsEditor, ListInput, Section } from './parts';
import { useEdited } from '@/lib/hooks';

const NONE: never[] = [];

const ERA_LABEL: Record<string, string> = {
  modern: 'Current MCP protocol',
  legacy: 'Earlier MCP protocol',
  unknown: 'Not checked',
};

const AUTH_LABEL: Record<AuthType, string> = {
  '': 'Headers only',
  oauth_client_credentials: 'OAuth client credentials',
  token_exchange: "Exchange the caller's token",
};

function filterText(f: { include?: string[]; exclude?: string[] } | undefined): string {
  if (!f) return 'All';
  const inc = f.include?.length ? f.include.join(', ') : 'all';
  return f.exclude?.length ? `${inc}, except ${f.exclude.join(', ')}` : inc;
}

/** Confirms "Approve current tools": what it records, and what it changes with pinning on. */
export function ApproveToolsDialog({ name, onClose }: { name: string | null; onClose: () => void }) {
  const approve = useMcp((s) => s.approveTools);
  const server = useMcp((s) => s.servers.data?.find((x) => x.name === name));
  const pinned = server?.spec.pin_tool_definitions === true;
  return (
    <ConfirmDialog
      open={name !== null}
      onClose={onClose}
      tone="primary"
      withReason={false}
      title={`Approve the current tools of ${name ?? ''}?`}
      objectName={name ?? undefined}
      confirmLabel="Approve tools"
      effect={
        pinned
          ? `The gateway asks ${name ?? 'the server'} for its tools now and records each definition. Tools hidden because their definition changed come back as they are now; a tool that changes later is hidden again until you approve it.`
          : `The gateway asks ${name ?? 'the server'} for its tools now and records each definition, so a later change is reported. Turn on pinning in the server's settings to also hide changed tools until they are approved.`
      }
      onConfirm={async () => {
        if (!name) return;
        try {
          const n = await approve(name);
          notify('success', 'Tools approved', `${plural(n, 'tool')} of ${name} recorded.`);
        } catch (e) {
          if (e instanceof ApiError && e.status === 502)
            throw new Error(
              `The gateway couldn't reach ${name} to read its tools. Check the server and try again.`,
            );
          throw e;
        }
      }}
    />
  );
}

function ServerView({ server, onApprove }: { server: McpServerResource; onApprove: () => void }) {
  const entry = useMcp((s) => s.statuses[server.name]);
  const gateways = useMcp((s) => s.gateways.data) ?? NONE;
  const canApprove = useCan('approveMcpServerTools');
  const status = entry?.status ?? null;
  const health = serverHealth(server, status);
  const s = server.spec;
  const used = gateways.filter((g) => g.spec.servers.includes(server.name)).map((g) => g.name);
  const headerNames = Object.keys(s.headers ?? {});
  const waiting = waitingTools(status);
  const fileOnly = s.transport === 'stdio';
  return (
    <div className="flex flex-col gap-6">
      {fileOnly ? (
        <Callout tone="info" title="Set in the configuration file">
          A server that runs as a local process can only be added or changed in the gateway&apos;s
          configuration file, so this page shows it without editing.
        </Callout>
      ) : null}
      {status?.last_contact_error ? (
        <Callout tone="error" title="The gateway couldn't reach this server">
          {status.last_contact_at ? `${capitalize(formatRelative(status.last_contact_at))}: ` : ''}
          {status.last_contact_error}. Check its URL and credentials; apps and agents don&apos;t see its tools
          meanwhile.
        </Callout>
      ) : null}
      {waiting > 0 ? (
        <Callout tone="warn" title={`${plural(waiting, 'tool')} waiting for approval`}>
          {s.pin_tool_definitions
            ? 'Apps and agents don’t see these tools until you approve their current definition.'
            : 'These definitions changed since the last approval. Check them, then approve.'}
        </Callout>
      ) : null}
      <Section title="Status" aside="This node">
        <Facts
          items={[
            {
              label: 'Status',
              mono: false,
              value: (
                <span className="flex items-center gap-1.5">
                  <Dot tone={HEALTH_TONE[health]} />
                  {HEALTH_LABEL[health]}
                </span>
              ),
            },
            {
              label: 'Last contact',
              mono: false,
              value: status?.last_contact_at
                ? `${formatRelative(status.last_contact_at)}${status.last_contact_error ? ', failed' : ', worked'}`
                : 'Not yet: no client used it',
            },
            {
              label: 'Protocol',
              mono: false,
              value: status
                ? `${ERA_LABEL[status.era_capability] ?? status.era_capability}${status.last_discovery_at ? ` · checked ${formatRelative(status.last_discovery_at)}` : ''}`
                : '—',
            },
            { label: 'Tools seen', value: status ? status.tools.length : '—' },
            {
              label: 'Tools approved',
              mono: false,
              value: status?.approved_at ? formatRelative(status.approved_at) : 'Never',
            },
            ...(status?.processes
              ? [
                  {
                    label: 'Processes',
                    value: `${status.processes.running} of ${status.processes.size} running · ${status.processes.started} started`,
                  },
                ]
              : []),
          ]}
        />
        {entry?.error && !status ? <p className="mt-2 text-caption text-error">{entry.error}</p> : null}
      </Section>
      <Section
        title="Tools"
        aside={
          canApprove && !fileOnly ? (
            <Button size="sm" icon="checkCircle" onClick={onApprove}>
              Approve current tools
            </Button>
          ) : status ? (
            plural(status.tools.length, 'tool')
          ) : undefined
        }
      >
        {status && status.tools.length > 0 ? (
          <Table label={`Tools of ${server.name}`}>
            <thead>
              <tr>
                <Th>Tool</Th>
                <Th>Approval</Th>
                <Th className="max-sm:hidden">Definition</Th>
              </tr>
            </thead>
            <tbody>
              {status.tools.map((t) => {
                const st = toolState(t);
                return (
                  <Tr key={t.name}>
                    <Td mono>{t.name}</Td>
                    <Td>
                      <Badge tone={st.tone} dot>
                        {st.label}
                      </Badge>
                    </Td>
                    <Td className="max-sm:hidden">
                      <span className="flex items-center gap-1">
                        <span className="mono text-caption text-muted">{shortId(t.fingerprint, 12)}</span>
                        <CopyButton value={t.fingerprint} label={`Copy the fingerprint of ${t.name}`} />
                      </span>
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : (
          <EmptyState
            size="sm"
            title={s.enabled === false ? 'This server is off' : 'No tools seen yet'}
            description={
              s.enabled === false
                ? 'Turn it on to list its tools.'
                : 'Tools show once a client lists them or you approve them.'
            }
          />
        )}
        <p className="mt-2 text-caption text-muted">
          Each definition is fingerprinted, so a tool whose description or schema changes is noticed.
          {s.pin_tool_definitions ? ' Pinning is on: a changed tool is hidden until approved.' : ''}
        </p>
      </Section>
      <Section title="Settings">
        <Facts
          items={[
            { label: 'Transport', mono: false, value: TRANSPORT_LABEL[s.transport] },
            ...(s.url ? [{ label: 'URL', value: s.url }] : []),
            ...(s.command ? [{ label: 'Command', value: [s.command, ...(s.args ?? [])].join(' ') }] : []),
            ...(s.openapi ? [{ label: 'OpenAPI document', value: s.openapi.spec }] : []),
            { label: 'Tool name prefix', value: `${s.prefix ?? server.name}__` },
            {
              label: 'Signs in with',
              mono: false,
              value:
                s.auth && s.auth.type !== 'none'
                  ? AUTH_LABEL[s.auth.type as AuthType]
                  : headerNames.length > 0
                    ? `Headers: ${headerNames.join(', ')} (values hidden)`
                    : 'Nothing',
            },
            { label: 'Tools offered', value: filterText(s.tools) },
            ...(s.prompts ? [{ label: 'Prompts offered', value: filterText(s.prompts) }] : []),
            ...(s.resources ? [{ label: 'Resources offered', value: filterText(s.resources) }] : []),
            ...(s.client_headers?.length
              ? [{ label: 'Headers clients may send', value: s.client_headers.join(', ') }]
              : []),
            { label: 'Pin tool definitions', mono: false, value: s.pin_tool_definitions ? 'On' : 'Off' },
            {
              label: 'Timeouts',
              mono: false,
              value:
                [
                  s.init_timeout ? `start ${s.init_timeout}` : null,
                  s.request_timeout ? `request ${s.request_timeout}` : null,
                  s.stream_idle_timeout ? `idle stream ${s.stream_idle_timeout}` : null,
                ]
                  .filter(Boolean)
                  .join(' · ') || 'Defaults',
            },
            { label: 'Gateways', mono: false, value: used.length > 0 ? used.join(', ') : 'None yet' },
          ]}
        />
      </Section>
    </div>
  );
}

function ServerEditor({
  values,
  set,
  errors,
  creating,
}: {
  values: ServerForm;
  set: <K extends keyof ServerForm>(key: K, value: ServerForm[K]) => void;
  errors: FieldErrors;
  creating: boolean;
}) {
  return (
    <div className="flex flex-col gap-5">
      {creating ? (
        <Field
          label="Name"
          required
          error={errors.name}
          hint="Tools appear to clients as name__tool unless you set a prefix. It can't be changed later."
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
      <Field label="Transport">
        <Segmented<Transport>
          label="Transport"
          value={values.transport}
          onChange={(v) => set('transport', v)}
          options={[
            { value: 'streamable_http', label: 'Streamable HTTP' },
            { value: 'sse', label: 'Server-sent events' },
            { value: 'openapi', label: 'OpenAPI' },
          ]}
        />
      </Field>
      <Field
        label={values.transport === 'openapi' ? 'API base URL' : 'URL'}
        required
        error={errors.url}
        hint={
          values.transport === 'openapi'
            ? 'Where the API’s operations are called.'
            : 'The server’s MCP endpoint.'
        }
      >
        <Input mono inputMode="url" value={values.url} onChange={(e) => set('url', e.target.value)} />
      </Field>
      {values.transport === 'openapi' ? (
        <>
          <Field
            label="OpenAPI document"
            required
            error={errors.openapiSpec}
            hint="A URL of the API's OpenAPI document."
          >
            <Input mono value={values.openapiSpec} onChange={(e) => set('openapiSpec', e.target.value)} />
          </Field>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Operations offered" optional hint="Operation ids or globs; all when empty.">
              <ListInput
                noun="operation"
                value={values.operationsInclude}
                onChange={(v) => set('operationsInclude', v)}
              />
            </Field>
            <Field label="Operations left out" optional>
              <ListInput
                noun="operation"
                value={values.operationsExclude}
                onChange={(v) => set('operationsExclude', v)}
              />
            </Field>
          </div>
        </>
      ) : null}
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Protocol">
          <Select
            value={values.protocol}
            onChange={(v) => set('protocol', v as ServerForm['protocol'])}
            options={[
              { value: 'auto', label: 'Detect' },
              { value: 'modern', label: 'Current' },
              { value: 'legacy', label: 'Earlier' },
            ]}
          />
        </Field>
        <Field label="Tool name prefix" optional error={errors.prefix} hint={`Defaults to the name.`}>
          <Input
            mono
            value={values.prefix}
            onChange={(e) => set('prefix', e.target.value)}
            placeholder={values.name || undefined}
          />
        </Field>
      </div>
      <Switch label="On" checked={values.enabled} onChange={(v) => set('enabled', v)} />

      <FormGroup title="Signing in to the server">
        <HeaderRowsEditor
          label="Headers sent to the server"
          rows={values.headers}
          onChange={(rows) => set('headers', rows)}
          error={errors.headers}
          hint="Values are stored encrypted and never shown again."
        />
        <Field label="Sign in with">
          <Select
            value={values.authType}
            onChange={(v) => set('authType', v as AuthType)}
            options={(Object.keys(AUTH_LABEL) as AuthType[]).map((a) => ({ value: a, label: AUTH_LABEL[a] }))}
          />
        </Field>
        {values.authType ? (
          <>
            <Field label="Token URL" required error={errors.tokenUrl}>
              <Input
                mono
                inputMode="url"
                value={values.tokenUrl}
                onChange={(e) => set('tokenUrl', e.target.value)}
              />
            </Field>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Client ID" required error={errors.clientId}>
                <Input
                  mono
                  autoComplete="off"
                  value={values.clientId}
                  onChange={(e) => set('clientId', e.target.value)}
                />
              </Field>
              <Field
                label="Client secret"
                required={values.authType === 'oauth_client_credentials' && !values.clientSecretStored}
                optional={values.authType === 'token_exchange'}
                error={errors.clientSecret}
              >
                <Input
                  mono
                  type="password"
                  autoComplete="off"
                  placeholder={values.clientSecretStored ? 'Stored, hidden. Type to replace' : undefined}
                  value={values.clientSecret}
                  onChange={(e) => set('clientSecret', e.target.value)}
                />
              </Field>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Send the client ID">
                <Select
                  value={values.clientAuth}
                  onChange={(v) => set('clientAuth', v as ServerForm['clientAuth'])}
                  options={[
                    { value: 'basic', label: 'As HTTP Basic' },
                    { value: 'post', label: 'In the form' },
                  ]}
                />
              </Field>
              <Field label="Scopes" optional>
                <ListInput noun="scope" value={values.scopes} onChange={(v) => set('scopes', v)} />
              </Field>
            </div>
            {values.authType === 'token_exchange' ? (
              <Field label="Audience" optional>
                <Input mono value={values.audience} onChange={(e) => set('audience', e.target.value)} />
              </Field>
            ) : null}
          </>
        ) : null}
      </FormGroup>

      <FormGroup
        title="What clients see"
        hint="Globs over the server's own names, such as get_* . Left out wins."
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="Tools offered" optional hint="All when empty.">
            <ListInput noun="tool" value={values.toolsInclude} onChange={(v) => set('toolsInclude', v)} />
          </Field>
          <Field label="Tools left out" optional>
            <ListInput noun="tool" value={values.toolsExclude} onChange={(v) => set('toolsExclude', v)} />
          </Field>
          <Field label="Prompts offered" optional>
            <ListInput
              noun="prompt"
              value={values.promptsInclude}
              onChange={(v) => set('promptsInclude', v)}
            />
          </Field>
          <Field label="Prompts left out" optional>
            <ListInput
              noun="prompt"
              value={values.promptsExclude}
              onChange={(v) => set('promptsExclude', v)}
            />
          </Field>
          <Field label="Resources offered" optional hint="Globs over resource URIs.">
            <ListInput
              noun="resource"
              value={values.resourcesInclude}
              onChange={(v) => set('resourcesInclude', v)}
            />
          </Field>
          <Field label="Resources left out" optional>
            <ListInput
              noun="resource"
              value={values.resourcesExclude}
              onChange={(v) => set('resourcesExclude', v)}
            />
          </Field>
        </div>
        <Field
          label="Headers clients may send"
          optional
          hint="Sent by a client as x-antirion-mcp-<server>-<header>."
        >
          <ListInput noun="header" value={values.clientHeaders} onChange={(v) => set('clientHeaders', v)} />
        </Field>
        <Switch
          label="Pin tool definitions: hide a tool whose definition changed until it is approved again"
          checked={values.pin}
          onChange={(v) => set('pin', v)}
        />
      </FormGroup>

      <FormGroup title="Timeouts and caching" hint="Leave empty for the gateway's defaults.">
        <div className="grid gap-4 sm:grid-cols-3">
          <Field label="Start" optional error={errors.initTimeout} hint="Default 10s">
            <Input mono value={values.initTimeout} onChange={(e) => set('initTimeout', e.target.value)} />
          </Field>
          <Field label="Request" optional error={errors.requestTimeout} hint="Default 120s">
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
          <Field label="Rediscover every" optional error={errors.discoveryTtl} hint="Default 5m">
            <Input mono value={values.discoveryTtl} onChange={(e) => set('discoveryTtl', e.target.value)} />
          </Field>
          <Field
            label="Keep tool lists"
            optional
            error={errors.toolCacheTtl}
            hint="Default 60s; 0s turns it off"
          >
            <Input mono value={values.toolCacheTtl} onChange={(e) => set('toolCacheTtl', e.target.value)} />
          </Field>
          <Field label="Retries" optional error={errors.retries} hint="0 to 3; default 1">
            <Input
              mono
              inputMode="numeric"
              value={values.retries}
              onChange={(e) => set('retries', e.target.value)}
            />
          </Field>
        </div>
      </FormGroup>
    </div>
  );
}

/**
 * One MCP server: its status, tools with their approval and settings, or the form that adds or
 * edits it. `server` null with `creating` adds a new one. Keyed by the caller on the server it
 * shows, so another server starts with a fresh form.
 */
export function ServerDrawer({
  server,
  creating,
  onClose,
  onCreated,
}: {
  server: McpServerResource | null;
  creating: boolean;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const servers = useMcp((s) => s.servers.data) ?? NONE;
  const gateways = useMcp((s) => s.gateways.data) ?? NONE;
  const save = useMcp((s) => s.saveServer);
  const remove = useMcp((s) => s.removeServer);
  const loadStatus = useMcp((s) => s.loadStatus);
  const canEdit = useCan('replaceMcpServer');
  const canDelete = useCan('deleteMcpServer');
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<ServerForm>(() =>
    server && !creating ? serverForm(server) : EMPTY_SERVER,
  );
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const edited = useEdited(values, creating || editing);
  const [approving, setApproving] = useState(false);
  const open = creating || server !== null;
  const form = creating || editing;
  const name = server?.name ?? null;

  useEffect(() => {
    if (name) void loadStatus(name);
  }, [name, loadStatus]);

  const set = <K extends keyof ServerForm>(key: K, value: ServerForm[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  };

  async function submit() {
    const found = serverErrors(
      values,
      servers.map((s) => s.name),
      creating,
    );
    if (Object.keys(found).length > 0) {
      setErrors(found);
      return;
    }
    const target = creating ? values.name.trim() : (server?.name ?? '');
    setBusy(true);
    setError(null);
    try {
      await save(target, serverSpec(values, creating ? null : server), creating ? null : server);
      notify('success', creating ? 'MCP server added' : 'MCP server saved', target);
      setEditing(false);
      if (creating) onCreated(target);
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `mcp.servers.${target}`));
    } finally {
      setBusy(false);
    }
  }

  const used = name ? gateways.filter((g) => g.spec.servers.includes(name)).map((g) => g.name) : [];
  const fileOnly = server?.spec.transport === 'stdio';

  return (
    <>
      <Drawer
        dirty={(creating || editing) && edited}
        open={open}
        onClose={onClose}
        width="lg"
        title={creating ? 'Add MCP server' : (server?.name ?? 'MCP server')}
        subtitle={
          creating
            ? 'A server whose tools, prompts and resources apps and agents reach through the gateway.'
            : server
              ? `${TRANSPORT_LABEL[server.spec.transport]}${server.spec.url ? ` · ${server.spec.url}` : ''}`
              : undefined
        }
        headerActions={
          server && !creating ? (
            <>
              <FavouriteButton kind="mcp_server" name={server.name} label={`MCP server ${server.name}`} />
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
                    if (server) setValues(serverForm(server));
                  }}
                >
                  Cancel
                </Button>
              )}
              <Button variant="primary" busy={busy} onClick={() => void submit()}>
                {creating ? 'Add MCP server' : 'Save changes'}
              </Button>
            </>
          ) : server && !fileOnly && (canEdit || canDelete) ? (
            <>
              {canDelete ? (
                <Button
                  variant="danger"
                  icon="trash"
                  disabled={used.length > 0}
                  disabledReason={used.length > 0 ? `Remove it from ${used.join(', ')} first` : undefined}
                  onClick={() => setDeleting(true)}
                  className="mr-auto"
                >
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
            <ServerEditor values={values} set={set} errors={errors} creating={creating} />
            <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
          </form>
        ) : server ? (
          <ServerView server={server} onApprove={() => setApproving(true)} />
        ) : null}
      </Drawer>
      <ApproveToolsDialog name={approving ? name : null} onClose={() => setApproving(false)} />
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title={`Delete MCP server ${name ?? ''}?`}
        objectName={name ?? undefined}
        typeToConfirm={name ?? undefined}
        confirmLabel="Delete server"
        effect="Apps and agents lose its tools, prompts and resources at once. Its approvals are forgotten."
        onConfirm={async (reason) => {
          if (!name) return;
          await remove(name, reason);
          notify('success', 'MCP server deleted', name);
          onClose();
        }}
      />
    </>
  );
}
