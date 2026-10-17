'use client';

import { useEffect, useMemo, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { useCan } from '@/components/ui/gate';
import { MenuButton } from '@/components/ui/menu';
import { EmptyState, ErrorState } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { API_BASE } from '@/lib/api/client';
import { formatCount } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { notify } from '@/stores/ui';

import { settingsApi } from '../api';
import { PROTOCOL_LABEL, type Protocol, type ProviderRow, providerRows, signInsByProvider } from '../sso';
import { useDirectory, useOrgSettings } from '../store';
import { Card } from './parts';
import { ProviderDrawer } from './provider-drawer';
import { idpState } from './saml-panel';

type Editing = { row: ProviderRow | { protocol: Protocol; name: null }; key: number } | null;

/** Where a row's sign-in comes from, in a few words. */
function source(row: ProviderRow): string {
  if (row.protocol === 'oidc') return (row.spec as { issuer: string }).issuer;
  if (row.protocol === 'oauth') {
    const orgs = (row.spec as { allowed_organizations?: string[] }).allowed_organizations ?? [];
    return orgs.length ? `Members of ${orgs.join(', ')}` : 'Any GitHub account with a mapping';
  }
  const idp = (row.spec as { idp: { metadata_url?: string; entity_id?: string } }).idp;
  return idp.metadata_url ?? idp.entity_id ?? 'Metadata pasted in';
}

/** The metadata state of each SAML provider, read once per list. */
function useSamlStates(rows: ProviderRow[], stamp: number) {
  const [states, setStates] = useState<Record<string, ReturnType<typeof idpState>>>({});
  const names = rows
    .filter((r) => r.protocol === 'saml')
    .map((r) => r.name)
    .join(',');
  useEffect(() => {
    if (!names) return;
    let live = true;
    for (const name of names.split(',')) {
      settingsApi.samlStatus(name).then(
        (s) => {
          if (live) setStates((m) => ({ ...m, [name]: idpState(s) }));
        },
        () => {
          if (live) setStates((m) => ({ ...m, [name]: { tone: 'error', text: "Status isn't available" } }));
        },
      );
    }
    return () => {
      live = false;
    };
  }, [names, stamp]);
  return states;
}

/**
 * Single sign-on providers (OIDC, SAML, GitHub): who signed in through each in the last 30 days, a
 * test sign-in, and add, edit and remove.
 */
export function IdentityProvidersSection() {
  const loadSettings = useOrgSettings((s) => s.loadSettings);
  const auth = useOrgSettings((s) => s.auth);
  const saveAuth = useOrgSettings((s) => s.saveAuth);
  const users = useDirectory((s) => s.users.data);
  const loadUsers = useDirectory((s) => s.loadUsers);
  const canWrite = useCan('patchAuthSettings');
  const canReadUsers = useCan('listUsers');
  const [editing, setEditing] = useState<Editing>(null);
  const [removing, setRemoving] = useState<ProviderRow | null>(null);
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);
  useEffect(() => {
    if (canReadUsers) void loadUsers();
  }, [canReadUsers, loadUsers]);
  const rows = useMemo(() => providerRows(auth.data?.spec.admin?.sso), [auth.data]);
  const now = useNow(60_000);
  const counts = useMemo(() => (users ? signInsByProvider(users, rows, now) : null), [users, rows, now]);
  const required = auth.data?.spec.admin?.local?.sso_required_domains ?? [];
  const open = (row: NonNullable<Editing>['row']) =>
    setEditing((prev) => ({ row, key: (prev?.key ?? 0) + 1 }));
  const [stamp, setStamp] = useState(0);
  const saml = useSamlStates(rows, stamp);
  const writeReason = canWrite ? undefined : "You don't have permission to change sign-in.";

  const addMenu = (
    <MenuButton
      label="Add provider"
      icon="plus"
      variant="primary"
      size="sm"
      items={(['oidc', 'saml', 'oauth'] as const).map((p) => ({
        label:
          p === 'oidc' ? 'OpenID Connect (Okta, Entra ID, Google…)' : p === 'saml' ? 'SAML 2.0' : 'GitHub',
        onSelect: () => open({ protocol: p, name: null }),
        disabled: !canWrite,
        disabledReason: writeReason,
      }))}
    />
  );

  return (
    <Card
      title="Single sign-on"
      description="Let people sign in with your identity provider."
      actions={addMenu}
    >
      {auth.error ? <ErrorState error={auth.error} subject="Single sign-on" size="sm" /> : null}
      {auth.data && rows.length === 0 ? (
        <EmptyState
          size="sm"
          title="No single sign-on yet"
          description="Add an OpenID Connect, SAML or GitHub provider so people use the accounts they have."
        />
      ) : null}
      {rows.length > 0 ? (
        <Table label="Identity providers">
          <thead>
            <tr>
              <Th>Provider</Th>
              <Th numeric>People, last 30 days</Th>
              <Th>
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const id = `${row.protocol}/${row.name}`;
              const testable = row.protocol !== 'oauth';
              const jit = (row.spec as { jit_provisioning?: boolean }).jit_provisioning ?? true;
              return (
                <Tr key={id} interactive onClick={() => open(row)}>
                  <Td className="w-full max-w-0 py-2 whitespace-normal">
                    <span className="flex items-center gap-2">
                      <span className="truncate font-medium text-fg">{row.displayName}</span>
                      <Badge>{PROTOCOL_LABEL[row.protocol]}</Badge>
                    </span>
                    <span className="mono block truncate text-caption text-muted">{source(row)}</span>
                    <span className="flex flex-wrap items-center gap-x-2 text-caption text-muted">
                      <span className="mono text-dim">{row.name}</span>
                      <span>· {jit ? 'Creates accounts at first sign-in' : 'Invited people only'}</span>
                      {saml[row.name] && row.protocol === 'saml' ? (
                        <span className="flex items-center gap-1.5">
                          · <Dot tone={saml[row.name]?.tone ?? 'neutral'} />
                          {saml[row.name]?.text}
                        </span>
                      ) : null}
                    </span>
                  </Td>
                  <Td numeric>{counts ? formatCount(counts.get(id) ?? 0) : '—'}</Td>
                  <Td>
                    <span
                      className="flex items-center justify-end gap-1"
                      onClick={(e) => e.stopPropagation()}
                    >
                      {testable ? (
                        <Tooltip
                          content={
                            canWrite
                              ? 'Opens your identity provider in a new tab. After you sign in there, the tab shows the claims, the mappings that matched and the roles a real sign-in would give. Nobody is signed in or created.'
                              : writeReason
                          }
                        >
                          <span>
                            <Button
                              size="sm"
                              icon="play"
                              disabled={!canWrite}
                              onClick={() =>
                                window.open(
                                  `${API_BASE}/auth/${row.protocol}/${encodeURIComponent(row.name)}/login?test=true`,
                                  '_blank',
                                  'noopener',
                                )
                              }
                            >
                              Test sign-in
                            </Button>
                          </span>
                        </Tooltip>
                      ) : null}
                      <MenuButton
                        label={`More for ${row.displayName}`}
                        iconOnly
                        variant="ghost"
                        size="sm"
                        items={[
                          { label: 'Edit', icon: 'edit', onSelect: () => open(row) },
                          {
                            label: 'Remove',
                            icon: 'trash',
                            danger: true,
                            disabled: !canWrite,
                            disabledReason: writeReason,
                            onSelect: () => setRemoving(row),
                          },
                        ]}
                      />
                    </span>
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
      {rows.length > 0 && !canReadUsers ? (
        <p className="mt-2 text-caption text-muted">Sign-in counts need permission to read people.</p>
      ) : null}
      {editing ? (
        <ProviderDrawer
          key={editing.key}
          open
          row={editing.row}
          taken={rows.filter((r) => r.protocol === editing.row.protocol).map((r) => r.name)}
          onClose={() => {
            setEditing(null);
            setStamp((n) => n + 1);
          }}
        />
      ) : null}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove ${removing?.displayName ?? ''}?`}
        objectName={removing?.name}
        confirmLabel="Remove provider"
        effect={
          rows.length === 1 && required.length > 0
            ? `It is the last single sign-on provider, and ${required.join(', ')} must use single sign-on. Remove those domains in Sign-in & sessions first.`
            : 'Its sign-in button disappears. People who used it keep their accounts and can sign in another way; their sessions stay open until they end.'
        }
        onConfirm={async (reason) => {
          if (!removing) return;
          await saveAuth({ admin: { sso: { [removing.protocol]: { [removing.name]: null } } } }, reason);
          notify('success', 'Provider removed', removing.displayName);
        }}
      />
    </Card>
  );
}
