'use client';

import Link from 'next/link';
import { useEffect } from 'react';

import { Badge } from '@/components/ui/badge';
import { Forbidden, useCan } from '@/components/ui/gate';
import { DefinitionGrid, Panel, TileGrid } from '@/components/ui/layout';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import { formatPercent, parseDuration, plural } from '@/lib/format';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import type { AuthSettings } from '../api';
import { coverage, isSso, mfaLabel, signInLabel } from '../facts';
import { useDirectory } from '../store';
import type { UserView } from '../types';

/** Defaults of the sign-in settings, shown when the configuration leaves them out. */
const DEFAULTS = { absolute: '12h', idle: '1h', trustedMaxAge: '30d', invitationTtl: '7d' };

/** "12h" → "12 hours", "90m" → "1.5 hours", "14d" → "14 days". */
export function humanDuration(value: string): string {
  const seconds = parseDuration(value);
  if (seconds === null) return value;
  const unit = (n: number, word: string) =>
    `${Number.isInteger(n) ? n : n.toFixed(1)} ${word}${n === 1 ? '' : 's'}`;
  if (seconds >= 86_400 && seconds % 3600 === 0) return unit(seconds / 86_400, 'day');
  if (seconds >= 3600) return unit(seconds / 3600, 'hour');
  if (seconds >= 60) return unit(seconds / 60, 'minute');
  return unit(seconds, 'second');
}

function duration(value: string | undefined, fallback: string): string {
  const text = humanDuration(value ?? fallback);
  return value ? text : `${text} (default)`;
}

interface Provider {
  id: string;
  name: string;
  kind: string;
  facts: string[];
}

/** The configured ways to sign in, by type and display name. */
export function providersOf(auth: AuthSettings['spec']): Provider[] {
  const admin = auth.admin ?? {};
  const out: Provider[] = [];
  for (const [id, p] of Object.entries(admin.sso?.oidc ?? {})) {
    let host = p.issuer;
    try {
      host = new URL(p.issuer).host;
    } catch {
      // The issuer is shown as written.
    }
    out.push({
      id,
      name: p.display_name ?? id,
      kind: 'OpenID Connect',
      facts: [
        host,
        p.allowed_email_domains?.length ? `only ${p.allowed_email_domains.join(', ')}` : 'any email domain',
        p.jit_provisioning === false ? 'accounts made ahead' : 'accounts made at first sign-in',
      ],
    });
  }
  for (const [id, p] of Object.entries(admin.sso?.oauth ?? {}))
    out.push({
      id,
      name: p.display_name ?? id,
      kind: 'GitHub',
      facts: [
        p.allowed_organizations?.length ? `members of ${p.allowed_organizations.join(', ')}` : 'any account',
        p.jit_provisioning === false ? 'accounts made ahead' : 'accounts made at first sign-in',
      ],
    });
  for (const id of Object.keys(admin.sso?.saml ?? {}))
    out.push({ id, name: id, kind: 'SAML', facts: ['not available on this gateway yet'] });
  if (admin.local?.enabled) {
    const factors = admin.local.mfa?.factors ?? ['totp', 'webauthn'];
    out.push({
      id: 'local',
      name: 'Email and password',
      kind: 'Password',
      facts: [
        `two-step with ${factors.map((f) => (f === 'totp' ? 'an authenticator app' : 'a security key')).join(' or ')}`,
        ...(admin.local.sso_required_domains?.length
          ? [`${admin.local.sso_required_domains.join(', ')} must use single sign-on`]
          : []),
      ],
    });
  }
  out.push({
    id: 'token',
    name: 'Admin tokens',
    kind: 'Token',
    facts: ['for scripts and the first sign-in'],
  });
  return out;
}

/** Identity providers, session lifetimes, SCIM and the directory's sign-in coverage. */
export function AccessPanel({ users }: { users: UserView[] }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const auth = useDirectory((s) => s.auth);
  const scim = useDirectory((s) => s.scim);
  const loadAuth = useDirectory((s) => s.loadAuth);
  const loadScim = useDirectory((s) => s.loadScim);
  const canAuth = useCan('getAuthSettings');
  const canScim = useCan('getScimStatus');
  const canEdit = useCan('patchAuthSettings');
  useEffect(() => {
    if (canAuth) void loadAuth();
    if (canScim) void loadScim();
  }, [canAuth, canScim, loadAuth, loadScim]);
  const cov = coverage(users);
  const people = users.filter((u) => u.state === 'active' && u.kind === 'human' && u.user);
  const withoutMfa = people.filter((u) => u.mfa.length === 0);
  const spec = auth.data?.spec;
  const session = spec?.admin?.session;
  const edit = canEdit ? (
    <Link
      href="/teams/?tab=access&settings=sign-in"
      className="text-ui font-medium text-accent hover:underline"
    >
      Edit sign-in settings
    </Link>
  ) : null;

  return (
    <TileGrid label="Access" className="lg:grid-cols-2">
      <Panel title="Ways to sign in" actions={edit}>
        {!canAuth ? <Forbidden what="sign-in settings" /> : null}
        {auth.error ? <ErrorState error={auth.error} subject="Sign-in settings" size="sm" /> : null}
        {canAuth && !auth.data && !auth.error ? <LoadingRows rows={3} /> : null}
        {spec ? (
          <ul aria-label="Ways to sign in" className="flex flex-col divide-y divide-line">
            {providersOf(spec).map((p) => (
              <li
                key={`${p.kind}-${p.id}`}
                className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-2 text-ui"
              >
                <span className="font-semibold text-fg">{p.name}</span>
                {p.kind !== p.name ? <Badge>{p.kind}</Badge> : null}
                <span className="w-full text-caption text-muted">{p.facts.join(' · ')}</span>
              </li>
            ))}
          </ul>
        ) : null}
      </Panel>

      <Panel title="Sessions">
        {spec ? (
          <DefinitionGrid
            columns={2}
            items={[
              { label: 'Signed out after', value: duration(session?.absolute_timeout, DEFAULTS.absolute) },
              { label: 'Idle for', value: duration(session?.idle_timeout, DEFAULTS.idle) },
              {
                label: 'Trusted browsers',
                value: session?.trusted_device?.enabled
                  ? `Up to ${duration(session.trusted_device.max_age, DEFAULTS.trustedMaxAge)}`
                  : 'Off',
              },
              {
                label: 'Invitation links last',
                value: duration(spec.admin?.invitations?.ttl, DEFAULTS.invitationTtl),
              },
            ]}
          />
        ) : canAuth ? (
          <LoadingRows rows={2} />
        ) : (
          <Forbidden what="session settings" />
        )}
      </Panel>

      <Panel title="Provisioning (SCIM)" subtitle="Your identity provider creates and removes accounts">
        {!canScim ? <Forbidden what="provisioning" /> : null}
        {scim.error ? <ErrorState error={scim.error} subject="Provisioning" size="sm" /> : null}
        {canScim && !scim.data && !scim.error ? <LoadingRows rows={2} /> : null}
        {scim.data ? (
          scim.data.enabled ? (
            <>
              <DefinitionGrid
                columns={3}
                items={[
                  { label: 'Users', value: scim.data.users },
                  { label: 'Groups', value: scim.data.groups },
                  {
                    label: 'Last request',
                    value: scim.data.last_request_at ? (
                      <Tooltip content={formatDateTime(scim.data.last_request_at, timeZone)}>
                        <span>{formatRelative(scim.data.last_request_at)}</span>
                      </Tooltip>
                    ) : (
                      'None yet'
                    ),
                  },
                ]}
              />
              {scim.data.last_error ? (
                <p className="mt-2 text-caption text-error">Last error: {scim.data.last_error}</p>
              ) : null}
              {spec?.admin?.scim?.group_mappings?.length ? (
                <p className="mt-2 text-caption text-muted">
                  {plural(spec.admin.scim.group_mappings.length, 'group')} give roles.
                </p>
              ) : null}
            </>
          ) : (
            <p className="text-ui text-muted">Off. Accounts are made by invitation or at first sign-in.</p>
          )
        ) : null}
      </Panel>

      <Panel title="Sign-in coverage" subtitle={plural(cov.people, 'active person', 'active people')}>
        {cov.people === 0 ? (
          <EmptyState size="sm" title="No active people yet" />
        ) : (
          <>
            <DefinitionGrid
              columns={2}
              items={[
                {
                  label: 'Last signed in with SSO',
                  value: `${formatPercent(cov.sso / cov.people, 0)} · ${cov.sso} of ${cov.people}`,
                },
                {
                  label: 'Two-step verification',
                  value: `${formatPercent(cov.mfa / cov.people, 0)} · ${cov.mfa} of ${cov.people}`,
                },
              ]}
            />
            <ul aria-label="People by sign-in" className="mt-3 flex flex-col gap-1 text-ui">
              {people.map((u) => (
                <li key={u.name} className="flex flex-wrap items-center gap-2">
                  <Link
                    href={`/teams/?user=${encodeURIComponent(u.name)}`}
                    className="min-w-0 truncate text-fg hover:underline"
                  >
                    {u.displayName}
                  </Link>
                  <span className="ml-auto text-caption text-muted">{signInLabel(u.signIn)}</span>
                  <Badge tone={u.mfa.length > 0 ? 'success' : 'warn'} dot>
                    {mfaLabel(u.mfa)}
                  </Badge>
                  {isSso(u.signIn) ? <Badge tone="success">SSO</Badge> : null}
                </li>
              ))}
            </ul>
            {withoutMfa.length > 0 ? (
              <p className="mt-2 text-caption text-muted">
                {plural(withoutMfa.length, 'person', 'people')} without two-step verification.
              </p>
            ) : null}
          </>
        )}
      </Panel>
    </TileGrid>
  );
}
