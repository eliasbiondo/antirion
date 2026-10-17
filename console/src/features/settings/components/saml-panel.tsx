'use client';

import { useCallback, useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { CopyButton } from '@/components/ui/misc';
import { ErrorState, LoadingRows } from '@/components/ui/states';
import { describeError } from '@/lib/api/client';
import { useNow } from '@/lib/hooks';
import { formatDate, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { type SamlStatus, settingsApi } from '../api';

const NAME_ID: Record<string, string> = {
  'urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress': 'Email address',
  'urn:oasis:names:tc:SAML:2.0:nameid-format:persistent': 'Persistent',
  'urn:oasis:names:tc:SAML:2.0:nameid-format:transient': 'Transient',
  'urn:oasis:names:tc:SAML:1.1:nameid-format:unspecified': 'Unspecified',
};

const DAY_MS = 86_400_000;

function Row({ label, value, copy }: { label: string; value: string; copy?: boolean }) {
  return (
    <div className="flex flex-col gap-0.5 py-1.5">
      <dt className="text-caption text-muted">{label}</dt>
      <dd className="flex items-start gap-1">
        <span className="mono min-w-0 flex-1 text-caption break-all text-fg">{value}</span>
        {copy ? <CopyButton value={value} label={`Copy the ${label.toLowerCase()}`} /> : null}
      </dd>
    </div>
  );
}

/** Whether the identity provider's metadata is fresh, failing, or not fetched yet, in words. */
export function idpState(s: SamlStatus): { tone: 'success' | 'warn' | 'error' | 'neutral'; text: string } {
  const idp = s.idp;
  if (idp.source !== 'metadata_url') return { tone: 'success', text: 'Configured by hand' };
  if (idp.last_error && !idp.last_sync_at) return { tone: 'error', text: "Can't fetch its metadata" };
  if (idp.last_error) return { tone: 'warn', text: 'Last refresh failed; the stored copy is in use' };
  if (!idp.last_sync_at) return { tone: 'neutral', text: 'Fetched at the first sign-in' };
  return { tone: 'success', text: `Metadata fetched ${formatRelative(idp.last_sync_at)}` };
}

/**
 * A SAML provider's two sides: what the identity provider needs of the gateway (entity ID, reply
 * address, name ID, the metadata file) and what the gateway holds of the identity provider (its
 * metadata address, last fetch, certificates), with "Fetch now".
 */
export function SamlPanel({ name }: { name: string }) {
  const [status, setStatus] = useState<SamlStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const canResync = useCan('resyncSamlProvider');
  const timeZone = usePreferences((s) => s.timeZone);
  const now = useNow(60_000);
  const load = useCallback(() => {
    settingsApi.samlStatus(name).then((s) => {
      setStatus(s);
      setError(null);
    }, setError);
  }, [name]);
  useEffect(load, [load]);
  if (error) return <ErrorState error={error} subject="The provider's status" size="sm" onRetry={load} />;
  if (!status) return <LoadingRows rows={3} />;
  const idp = status.idp;
  const state = idpState(status);
  return (
    <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
      <section className="flex flex-col rounded-control border border-line bg-sunken px-3.5 py-3">
        <h4 className="flex items-center gap-2 text-ui font-semibold text-fg">
          <Icon name="server" size={14} className="text-muted" />
          This gateway
        </h4>
        <p className="mb-1 text-caption text-muted">Give these to your identity provider.</p>
        <dl className="flex flex-col divide-y divide-line">
          <Row label="Entity ID" value={status.sp_entity_id} copy />
          <Row label="Reply (ACS) URL" value={status.acs_url} copy />
          <Row label="Name ID" value={NAME_ID[status.name_id_format] ?? status.name_id_format} />
        </dl>
        <a
          href={status.sp_metadata_url}
          download={`${name}-sp-metadata.xml`}
          className="mt-2 inline-flex w-fit items-center gap-1.5 text-ui text-accent hover:underline"
        >
          <Icon name="download" size={14} />
          Download metadata
        </a>
      </section>
      <section className="flex flex-col rounded-control border border-line bg-sunken px-3.5 py-3">
        <h4 className="flex items-center gap-2 text-ui font-semibold text-fg">
          <Icon name="lock" size={14} className="text-muted" />
          Identity provider
        </h4>
        <p className="mb-1 flex items-center gap-1.5 text-caption text-muted">
          <Dot tone={state.tone} />
          {state.text}
        </p>
        <dl className="flex flex-col divide-y divide-line">
          {idp.metadata_url ? <Row label="Metadata address" value={idp.metadata_url} copy /> : null}
          <Row label="Entity ID" value={idp.entity_id ?? 'Not known until fetched'} />
          {idp.sso_url ? <Row label="Sign-in address" value={idp.sso_url} /> : null}
        </dl>
        {idp.last_error ? <p className="mt-1 text-caption text-error">{idp.last_error}</p> : null}
        {idp.certificates.length > 0 ? (
          <ul className="mt-2 flex flex-col gap-1">
            {idp.certificates.map((c) => {
              const end = c.not_after ? Date.parse(c.not_after) : null;
              const tone =
                end === null ? 'neutral' : end < now ? 'error' : end - now < 30 * DAY_MS ? 'warn' : 'neutral';
              return (
                <li key={c.sha256} className="flex flex-wrap items-center gap-2 text-caption">
                  <Icon name="key" size={12} className="text-muted" />
                  <span className="text-fg">{c.subject ?? 'Certificate'}</span>
                  <span className="mono text-dim">{c.sha256.slice(0, 16)}…</span>
                  {end !== null ? (
                    <Badge tone={tone}>
                      {end < now ? 'Expired' : 'Expires'} {formatDate(c.not_after ?? '', timeZone)}
                    </Badge>
                  ) : null}
                </li>
              );
            })}
          </ul>
        ) : null}
        {idp.source === 'metadata_url' ? (
          <div className="mt-2">
            <Button
              size="sm"
              icon="refresh"
              busy={busy}
              disabled={!canResync}
              disabledReason={
                canResync ? undefined : "You don't have permission to fetch the identity provider's details."
              }
              onClick={() => {
                setBusy(true);
                settingsApi
                  .samlResync(name)
                  .then(
                    (s) => {
                      setStatus(s);
                      notify('success', 'Metadata fetched', name);
                    },
                    (e: unknown) => {
                      notify('error', "Couldn't fetch the metadata", describeError(e));
                      load();
                    },
                  )
                  .finally(() => setBusy(false));
              }}
            >
              Fetch metadata now
            </Button>
          </div>
        ) : null}
      </section>
    </div>
  );
}
