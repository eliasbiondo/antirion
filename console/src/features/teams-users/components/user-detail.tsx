'use client';

import { useEffect, useState } from 'react';

import { FavouriteButton } from '@/components/ui/favourite';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Select } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { DetailHeader, DetailSection } from '@/components/ui/list-detail';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { Avatar } from '@/components/ui/misc';
import { Banner, EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { useNow } from '@/lib/hooks';
import { formatDate, formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { activityLabel, isInactive } from '../facts';
import { useSeats } from '../hooks';
import { useDirectory, userActions, useUsers } from '../store';
import {
  invitationExpired,
  roleLabel,
  scopeLabel,
  STATE_LABEL,
  type TeamView,
  topRole,
  type UserView,
} from '../types';
import { BindingDialog } from './forms';
import {
  FlagBanner,
  InactiveBanner,
  PermissionsSection,
  SessionsSection,
  signInItems,
  SignInBadges,
  TeamsSection,
  UsageSection,
} from './user-facts';
import { auditActorBy } from '@/lib/audit-words';
import { useTokenNames } from '@/stores/actors';

type DialogKind =
  | 'binding'
  | 'token'
  | 'delete'
  | 'disable'
  | 'sessions'
  | 'mfa'
  | 'offboard'
  | 'rotate-keys'
  | 'cancel-invite'
  | null;

export interface UserDetailProps {
  view: UserView;
  teams: TeamView[];
  onEdit: () => void;
  onSecret: (title: string, value: string) => void;
}

/** A user: identity, role bindings, admin tokens, keys owned, activity and the administration actions. */
export function UserDetail({ view, teams, onEdit, onSecret }: UserDetailProps) {
  const tokenNames = useTokenNames();
  const timeZone = usePreferences((s) => s.timeZone);
  const now = useNow(60_000);
  const seats = useSeats();
  const tokens = useDirectory((s) => s.tokens[view.name]);
  const audit = useDirectory((s) => s.audit[view.name]);
  const keys = useDirectory((s) => s.keys.data);
  const loadTokens = useDirectory((s) => s.loadTokens);
  const loadAudit = useDirectory((s) => s.loadAudit);
  const removeBinding = useDirectory((s) => s.removeBinding);
  const createToken = useDirectory((s) => s.createToken);
  const deleteToken = useDirectory((s) => s.deleteToken);
  const cancelInvitation = useDirectory((s) => s.cancelInvitation);
  const resendInvitation = useDirectory((s) => s.resendInvitation);
  const update = useUsers((s) => s.update);
  const remove = useUsers((s) => s.remove);
  const [dialog, setDialog] = useState<DialogKind>(null);
  const [tokenDraft, setTokenDraft] = useState({ description: '', days: '90' });
  const [offboardKeys, setOffboardKeys] = useState<'disable' | 'delete' | 'transfer'>('disable');
  const [transferTo, setTransferTo] = useState('');
  const owned = (keys ?? []).filter((k) => k.spec.owner === view.name);
  const role = topRole(view);
  const user = view.user;
  const canMfa = useCan('resetUserMfa');
  const canSessions = useCan('revokeUserSessions');
  const canRotate = useCan('rotateKey');
  const canPatch = useCan('patchUser');
  const canOffboard = useCan('offboardUser');
  const canDelete = useCan('deleteUser');

  useEffect(() => {
    if (!user) return;
    void loadTokens(view.name);
    void loadAudit(view.name);
  }, [user, view.name, loadTokens, loadAudit]);

  if (!user || view.invitation) {
    const inv = view.invitation;
    return (
      <div>
        <DetailHeader
          eyebrow={
            inv && invitationExpired(inv, now) ? (
              <Badge tone="warn" dot>
                Invitation expired
              </Badge>
            ) : (
              <Badge tone="info" dot>
                Invited
              </Badge>
            )
          }
          title={view.email}
          subtitle={inv ? `Invited by ${inv.invited_by} · ${formatRelative(inv.created_at)}` : undefined}
          actions={
            inv ? (
              <>
                <Gate operation="resendInvitation">
                  <Button
                    variant="primary"
                    icon="mail"
                    onClick={() =>
                      void resendInvitation(inv.id).then(() =>
                        notify('success', 'Invitation sent again', view.email),
                      )
                    }
                  >
                    Resend invitation
                  </Button>
                </Gate>
                <Gate operation="cancelInvitation">
                  <Button variant="secondary" onClick={() => setDialog('cancel-invite')}>
                    Withdraw invitation
                  </Button>
                </Gate>
              </>
            ) : null
          }
        />
        {inv ? (
          <>
            {invitationExpired(inv, now) ? (
              <Banner tone="warn" title="This invitation has expired">
                The link no longer works. Resend the invitation to send a new one.
              </Banner>
            ) : null}
            <DetailSection title="Invitation">
              <DefinitionGrid
                columns={3}
                items={[
                  { label: 'Invited by', value: inv.invited_by },
                  {
                    label: 'Sent',
                    value: (
                      <Tooltip content={formatDateTime(inv.created_at, timeZone)}>
                        <span>{formatDate(inv.created_at, timeZone)}</span>
                      </Tooltip>
                    ),
                  },
                  {
                    label: invitationExpired(inv, now) ? 'Link expired' : 'Link expires',
                    value: (
                      <Tooltip content={formatDateTime(inv.expires_at, timeZone)}>
                        <span>
                          {formatDate(inv.expires_at, timeZone)} ({formatRelative(inv.expires_at)})
                        </span>
                      </Tooltip>
                    ),
                  },
                ]}
              />
            </DetailSection>
            <DetailSection title="Roles when they join">
              {(inv.role_bindings ?? []).length === 0 ? (
                <p className="text-ui text-muted">None. Assign roles once they have joined.</p>
              ) : (
                <ul className="flex flex-col gap-1.5">
                  {(inv.role_bindings ?? []).map((b) => (
                    <li key={`${b.role}@${b.scope}`} className="flex items-center gap-2 text-ui">
                      <Badge tone="warn" title={b.role}>
                        {roleLabel(b.role)}
                      </Badge>
                      <span className="text-muted">{scopeLabel(b.scope)}</span>
                    </li>
                  ))}
                </ul>
              )}
            </DetailSection>
          </>
        ) : null}
        <ConfirmDialog
          open={dialog === 'cancel-invite'}
          onClose={() => setDialog(null)}
          title={`Withdraw the invitation to ${view.email}?`}
          effect="The link stops working. You can invite them again later."
          confirmLabel="Withdraw invitation"
          withReason={false}
          onConfirm={async () => {
            if (!inv) return;
            await cancelInvitation(inv.id);
            notify('success', 'Invitation withdrawn', view.email);
          }}
        />
      </div>
    );
  }

  const inactive = isInactive(view, now);
  const more: MenuItem[] = [
    {
      label: 'Copy link',
      icon: 'link',
      onSelect: () => void navigator.clipboard.writeText(window.location.href),
    },
  ];
  if (canMfa)
    more.push({ label: 'Reset two-step verification', icon: 'lock', onSelect: () => setDialog('mfa') });
  if (canSessions)
    more.push({ label: 'Sign out everywhere', icon: 'logout', onSelect: () => setDialog('sessions') });
  if (canRotate)
    more.push({
      label: 'Rotate their keys',
      icon: 'rotate',
      disabled: owned.length === 0,
      disabledReason: "They don't own any keys",
      onSelect: () => setDialog('rotate-keys'),
    });
  const blockedBySeats = view.kind === 'human' && seats.full;
  if (canPatch)
    more.push(
      view.state === 'disabled'
        ? {
            label: 'Enable',
            icon: 'play',
            disabled: blockedBySeats,
            disabledReason: `All ${seats.cap ?? ''} seats are in use. Disable someone or raise the seat cap first.`,
            onSelect: () =>
              void update(view.name, { ...user.spec, enabled: true }).then(() =>
                notify('success', 'User enabled', view.name),
              ),
          }
        : { label: 'Disable', icon: 'pause', onSelect: () => setDialog('disable') },
    );
  if (canOffboard)
    more.push({ label: 'Offboard', icon: 'logout', danger: true, onSelect: () => setDialog('offboard') });
  if (canDelete)
    more.push({ label: 'Delete', icon: 'trash', danger: true, onSelect: () => setDialog('delete') });

  return (
    <div>
      <DetailHeader
        eyebrow={
          <>
            <Badge tone={view.state === 'active' ? 'success' : 'neutral'} dot>
              {STATE_LABEL[view.state]}
            </Badge>
            {view.flagged ? (
              <Badge tone="warn" dot>
                Flagged
              </Badge>
            ) : null}
            {inactive ? <Badge dot>Inactive</Badge> : null}
            {role ? <Badge tone="warn">{roleLabel(role)}</Badge> : null}
            {view.kind === 'service' ? <Badge>Service account</Badge> : null}
            <SignInBadges view={view} />
          </>
        }
        title={
          <span className="flex items-center gap-3">
            <Avatar name={view.displayName} size="md" />
            <span className="truncate">{view.displayName}</span>
            <FavouriteButton kind="user" name={view.name} label={view.displayName} />
          </span>
        }
        subtitle={[view.name, view.email, user.spec.title].filter(Boolean).join(' · ')}
        actions={
          <>
            <Gate operation="replaceUser">
              <Button variant="primary" icon="edit" onClick={onEdit}>
                Edit
              </Button>
            </Gate>
            <span className="ml-auto">
              <MenuButton items={more} />
            </span>
          </>
        }
      />
      {view.state === 'disabled' ? (
        <Banner tone="warn" title="This user is disabled">
          They can&apos;t sign in. Keys they own keep working unless you disable them too.
        </Banner>
      ) : null}
      <FlagBanner view={view} />
      {inactive ? <InactiveBanner view={view} onDisable={() => setDialog('disable')} /> : null}

      <DetailSection title="Details">
        <DefinitionGrid
          columns={3}
          items={[
            { label: 'Username', value: view.name },
            { label: 'Type', value: view.kind === 'service' ? 'Service account' : 'Person' },
            { label: 'Team', value: view.primaryTeam ?? '—' },
            { label: 'Email', value: view.email || '—' },
            { label: 'Joined', value: formatDate(user.metadata.created_at, timeZone) },
            { label: 'Keys owned', value: owned.length },
            ...(view.kind === 'human' ? signInItems(view, timeZone) : []),
          ]}
        />
      </DetailSection>

      <UsageSection view={view} />
      <TeamsSection view={view} teams={teams} />

      <DetailSection
        title="Roles"
        aside={
          <Gate operation="createRoleBinding" mode="hide">
            <Button size="sm" icon="plus" onClick={() => setDialog('binding')}>
              Assign role
            </Button>
          </Gate>
        }
      >
        {view.bindings.length === 0 ? (
          <EmptyState size="sm" title="No roles yet" description="They can sign in but won't see anything." />
        ) : (
          <ul className="flex flex-col gap-1.5">
            {view.bindings.map((b) => (
              <li key={`${b.role}@${b.scope}`} className="flex items-center gap-2 text-ui">
                <Badge tone="warn" title={b.role}>
                  {roleLabel(b.role)}
                </Badge>
                <span className="text-muted">{scopeLabel(b.scope)}</span>
                {b.id ? (
                  <Gate operation="deleteRoleBinding" mode="hide">
                    <Button
                      size="sm"
                      variant="ghost"
                      className="ml-auto"
                      onClick={() =>
                        void removeBinding(b.id ?? '').then(() =>
                          notify('success', 'Role removed', `${roleLabel(b.role)} from ${view.name}`),
                        )
                      }
                    >
                      Remove
                    </Button>
                  </Gate>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </DetailSection>

      <PermissionsSection view={view} />

      <DetailSection
        title="Admin tokens"
        aside={
          <Gate operation="createAdminToken" mode="hide">
            <Button size="sm" icon="plus" onClick={() => setDialog('token')}>
              Create token
            </Button>
          </Gate>
        }
      >
        {tokens?.status === 'loading' && !tokens.data ? <LoadingRows rows={2} /> : null}
        {tokens?.error ? <ErrorState error={tokens.error} subject="Admin tokens" /> : null}
        {tokens?.data?.length === 0 ? (
          <p className="text-ui text-muted">None. Admin tokens let scripts use the Admin API as this user.</p>
        ) : null}
        {tokens?.data && tokens.data.length > 0 ? (
          <Table label={`Admin tokens of ${view.name}`}>
            <thead>
              <tr>
                <Th>Token</Th>
                <Th>Description</Th>
                <Th>Expires</Th>
                <Th>Last used</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {tokens.data.map((t) => (
                <Tr key={t.kid}>
                  <Td mono>{t.hint}</Td>
                  <Td muted>{t.description ?? '—'}</Td>
                  <Td muted>{t.expires_at ? formatDate(t.expires_at, timeZone) : 'Never'}</Td>
                  <Td muted>{formatRelative(t.last_used_at ?? null)}</Td>
                  <Td className="text-right">
                    <Gate operation="deleteAdminToken" mode="hide">
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          void deleteToken(view.name, t.kid).then(() =>
                            notify('success', 'Token revoked', t.hint),
                          )
                        }
                      >
                        Revoke
                      </Button>
                    </Gate>
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        ) : null}
      </DetailSection>

      <SessionsSection view={view} onSignOut={() => setDialog('sessions')} />

      <DetailSection title="Keys they own">
        {owned.length === 0 ? (
          <p className="text-ui text-muted">None</p>
        ) : (
          <ul className="mono flex flex-wrap gap-1.5 text-caption">
            {owned.map((k) => (
              <li key={k.name}>
                <Badge>{k.name}</Badge>
              </li>
            ))}
          </ul>
        )}
      </DetailSection>

      <DetailSection title="Activity">
        {audit?.status === 'loading' && !audit.data ? <LoadingRows rows={3} /> : null}
        {audit?.error ? <ErrorState error={audit.error} subject="The audit log" /> : null}
        {audit?.data?.length === 0 ? <EmptyState size="sm" title="Nothing recorded yet" /> : null}
        <ol className="flex flex-col">
          {(audit?.data ?? []).slice(0, 20).map((e) => (
            <li
              key={e.id}
              className="flex items-center gap-3 border-b border-line py-2 text-ui last:border-0"
            >
              <Dot tone={e.outcome === 'success' ? 'success' : 'error'} />
              <Tooltip content={e.action}>
                <span className="text-fg">{activityLabel(e)}</span>
              </Tooltip>
              <span className="mono truncate text-muted">{e.resource}</span>
              <span className="text-muted">by {auditActorBy(e.actor, tokenNames)}</span>
              <span className="ml-auto text-caption text-dim">{formatRelative(e.time)}</span>
            </li>
          ))}
        </ol>
      </DetailSection>

      {dialog === 'binding' ? <BindingDialog user={view.name} onClose={() => setDialog(null)} /> : null}
      <ConfirmDialog
        open={dialog === 'token'}
        onClose={() => setDialog(null)}
        title={`Create an admin token for ${view.name}`}
        tone="primary"
        confirmLabel="Create token"
        withReason={false}
        effect="The token acts with this user's roles. You'll see it once."
        onConfirm={async () => {
          const value = await createToken(
            view.name,
            tokenDraft.description,
            tokenDraft.days === '0' ? null : Number(tokenDraft.days),
          );
          onSecret(`Admin token for ${view.name}`, value);
        }}
      >
        <Field label="Description" optional>
          <Input
            placeholder="e.g. Deploy script"
            value={tokenDraft.description}
            onChange={(e) => setTokenDraft({ ...tokenDraft, description: e.target.value })}
          />
        </Field>
        <Field label="Expires">
          <Select
            value={tokenDraft.days}
            onChange={(v) => setTokenDraft({ ...tokenDraft, days: v })}
            options={[
              { value: '30', label: 'In 30 days' },
              { value: '90', label: 'In 90 days' },
              { value: '365', label: 'In 1 year' },
              { value: '0', label: 'Never' },
            ]}
          />
        </Field>
      </ConfirmDialog>
      <ConfirmDialog
        open={dialog === 'disable'}
        onClose={() => setDialog(null)}
        title={`Disable ${view.name}?`}
        tone="primary"
        confirmLabel="Disable user"
        effect="They won't be able to sign in, and they'll be signed out now. Their admin tokens stop working until you enable them again."
        onConfirm={async () => {
          await update(view.name, { ...user.spec, enabled: false });
          notify('success', 'User disabled', view.name, {
            label: 'Undo',
            onClick: () => void update(view.name, { ...user.spec, enabled: true }),
          });
        }}
      />
      <ConfirmDialog
        open={dialog === 'delete'}
        onClose={() => setDialog(null)}
        title={`Delete ${view.name}?`}
        confirmLabel="Delete user"
        typeToConfirm={view.name}
        effect="The account, its roles and its admin tokens are removed, and keys it owns are disabled. This can't be undone."
        onConfirm={async (reason) => {
          await remove(view.name, reason);
          notify('success', 'User deleted', view.name);
        }}
      />
      <ConfirmDialog
        open={dialog === 'sessions'}
        onClose={() => setDialog(null)}
        title={`Sign ${view.name} out everywhere?`}
        tone="primary"
        confirmLabel="Sign out everywhere"
        effect="Every console session they have ends now. They'll need to sign in again."
        onConfirm={async () => {
          await userActions.revokeSessions(view.name);
          void useDirectory.getState().loadSessions(view.name);
          notify('success', 'Signed out everywhere', view.name);
        }}
      />
      <ConfirmDialog
        open={dialog === 'mfa'}
        onClose={() => setDialog(null)}
        title={`Reset two-step verification for ${view.name}?`}
        confirmLabel="Reset two-step verification"
        effect="Their authenticator apps, security keys and recovery codes are removed. They'll set up two-step verification again the next time they sign in."
        onConfirm={async (reason) => {
          await userActions.resetMfa(view.name, reason);
          notify('success', 'Two-step verification reset', view.name);
        }}
      />
      <ConfirmDialog
        open={dialog === 'rotate-keys'}
        onClose={() => setDialog(null)}
        title={
          owned.length === 1 ? `Rotate ${view.name}'s key?` : `Rotate ${view.name}'s ${owned.length} keys?`
        }
        tone="primary"
        confirmLabel={owned.length === 1 ? 'Rotate key' : 'Rotate keys'}
        withReason={false}
        effect="Each key gets a new secret, shown to you once so you can pass it on. The old secrets keep working for 24 hours."
        onConfirm={async () => {
          const rotated = await userActions.rotateOwnedKeys(view.name);
          onSecret(
            `New secrets for ${view.name}'s keys`,
            rotated.map((r) => `${r.key}: ${r.secret}`).join('\n'),
          );
        }}
      />
      <ConfirmDialog
        open={dialog === 'offboard'}
        onClose={() => setDialog(null)}
        title={`Offboard ${view.name}?`}
        confirmLabel="Offboard user"
        typeToConfirm={view.name}
        effect="Disables the account, signs them out, deletes their admin tokens and handles their keys as you choose below."
        onConfirm={async (reason) => {
          await userActions.offboard(view.name, {
            keys: offboardKeys,
            ...(offboardKeys === 'transfer' ? { transfer_to: transferTo } : {}),
            ...(reason ? { reason } : {}),
          });
          notify('success', 'User offboarded', view.name);
        }}
      >
        <Field label="Keys they own">
          <Select
            value={offboardKeys}
            onChange={(v) => setOffboardKeys(v as typeof offboardKeys)}
            options={[
              { value: 'disable', label: 'Disable them' },
              { value: 'delete', label: 'Delete them' },
              { value: 'transfer', label: 'Transfer them' },
            ]}
          />
        </Field>
        {offboardKeys === 'transfer' ? (
          <Field label="Transfer to" hint="The username of the new owner.">
            <Input mono value={transferTo} onChange={(e) => setTransferTo(e.target.value)} />
          </Field>
        ) : null}
      </ConfirmDialog>
    </div>
  );
}
