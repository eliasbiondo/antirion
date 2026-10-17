'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { useCan } from '@/components/ui/gate';
import { CopyButton } from '@/components/ui/misc';
import { ErrorState, LoadingRows } from '@/components/ui/states';
import { formatDate } from '@/lib/time';
import { daysUntil } from '@/lib/time';
import type { components } from '@/lib/api/schema';
import { usePreferences } from '@/stores/preferences';

import { type EncryptionReport, settingsApi } from '../api';
import { Card, Facts } from './parts';

type ListenerTls = components['schemas']['ListenerTlsReport'];

const CLIENT_AUTH: Record<ListenerTls['client_auth'], string> = {
  none: 'Not asked for',
  optional: 'Asked for, not required',
  required: 'Required',
};

function versions(tls: ListenerTls): string {
  return tls.versions.map((v) => `TLS ${v}`).join(' and ');
}

function Certificate({ tls }: { tls: ListenerTls }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const cert = tls.certificate;
  if (!cert) return <span className="text-muted">Couldn&apos;t read the certificate file</span>;
  const days = daysUntil(cert.not_after);
  return (
    <span className="flex flex-col gap-0.5">
      <span className="flex flex-wrap items-center gap-2">
        <span className="mono">{cert.subject ?? cert.dns_names[0] ?? 'No subject'}</span>
        {days !== null && days <= 0 ? (
          <Badge tone="error">Expired</Badge>
        ) : days !== null && days <= 21 ? (
          <Badge tone="warn">Expires in {days} days</Badge>
        ) : null}
      </span>
      <span className="text-caption text-muted">
        Issued by {cert.issuer ?? 'an unnamed issuer'} · valid until {formatDate(cert.not_after, timeZone)}
        {cert.dns_names.length > 0 ? ` · for ${cert.dns_names.join(', ')}` : ''}
      </span>
      <span className="flex items-center gap-1 text-caption text-dim">
        <span className="mono truncate">SHA-256 {cert.sha256.slice(0, 23)}…</span>
        <CopyButton value={cert.sha256} label="Copy the certificate fingerprint" />
      </span>
    </span>
  );
}

function listenerFacts(label: string, tls: ListenerTls) {
  if (!tls.enabled)
    return [
      {
        label,
        value: 'Off on the gateway',
        hint: 'Usually handled by a load balancer in front of it.',
      },
    ];
  return [
    { label, value: `On, ${versions(tls)}`, hint: `Client certificates: ${CLIENT_AUTH[tls.client_auth]}` },
    { label: `${label}: certificate`, value: <Certificate tls={tls} /> },
  ];
}

/** The serving node's encryption report: TLS on each listener with its certificate, and secrets at rest. */
export function EncryptionCard() {
  const canRead = useCan('getEncryption');
  const [report, setReport] = useState<EncryptionReport | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    if (!canRead) return;
    settingsApi.encryption().then(setReport, setError);
  }, [canRead]);
  if (!canRead) return null;
  const rest = report?.at_rest;
  return (
    <Card
      title="Encryption"
      description="How traffic and stored secrets are protected, as this node reports it."
    >
      {error ? <ErrorState error={error} subject="The encryption report" size="sm" /> : null}
      {!report && !error ? <LoadingRows rows={3} /> : null}
      {report && rest ? (
        <Facts
          items={[
            ...listenerFacts('App traffic', report.inbound_tls.data),
            ...listenerFacts('This console', report.inbound_tls.admin),
            ...(report.inbound_tls.data.enabled || report.inbound_tls.admin.enabled
              ? [
                  {
                    label: 'Certificates reloaded',
                    value: <span className="mono">every {report.inbound_tls.reload_interval}</span>,
                  },
                ]
              : []),
            {
              label: 'Stored secrets',
              value: `Encrypted with ${rest.algorithm}`,
              hint: 'Provider keys, signing secrets and sessions saved through the console or API.',
            },
            {
              label: 'Encryption keys',
              value: (
                <span className="flex flex-col gap-1">
                  {rest.keys.map((k) => (
                    <span key={k.id} className="flex flex-wrap items-center gap-2">
                      <span className="mono">{k.id}</span>
                      {k.primary ? (
                        <Badge tone="accent">Encrypts new secrets</Badge>
                      ) : (
                        <Badge>Decrypts only</Badge>
                      )}
                      <span className="text-caption text-muted">
                        {k.source === 'configured'
                          ? 'From the startup configuration'
                          : 'Generated in the data directory'}
                      </span>
                    </span>
                  ))}
                </span>
              ),
              hint: 'Each key is shown by a short fingerprint, never the key itself.',
            },
            {
              label: 'External secret stores',
              value:
                rest.external_stores.length > 0
                  ? rest.external_stores.map((s) => s.name).join(', ')
                  : 'None: secrets are kept in the gateway’s store',
            },
          ]}
        />
      ) : null}
    </Card>
  );
}
