'use client';

import { useEffect, useState } from 'react';

import { useCan } from '@/components/ui/gate';
import { Switch } from '@/components/ui/form';
import { CopyButton } from '@/components/ui/misc';
import { ErrorState, InlineError } from '@/components/ui/states';
import { api } from '@/lib/api/client';
import { notify } from '@/stores/ui';

import { Card, Facts } from './parts';

type McpSettings = Awaited<ReturnType<typeof readMcp>>;

function readMcp() {
  return api.get('/settings/mcp');
}

/**
 * The built-in read-only MCP server on the admin listener: on or off, its address, and what its
 * tools may read (each tool reads as the signed-in caller).
 */
export function AdminMcpCard() {
  const canRead = useCan('getMcpSettings');
  const canEdit = useCan('patchMcpSettings');
  const [settings, setSettings] = useState<McpSettings | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (canRead) readMcp().then(setSettings, setLoadError);
  }, [canRead]);
  if (!canRead) return null;
  const on = settings?.spec.admin_server?.enabled === true;
  const address = typeof window === 'undefined' ? '/admin/mcp' : `${window.location.origin}/admin/mcp`;

  async function toggle(next: boolean) {
    setBusy(true);
    setError(null);
    try {
      await api.patch('/settings/mcp', {
        body: { spec: { admin_server: { enabled: next } } },
        ifMatch: settings?.metadata.etag,
      });
      setSettings(await readMcp());
      notify('success', next ? 'Admin MCP server turned on' : 'Admin MCP server turned off');
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card
      title="Admin MCP server"
      description="Lets an AI assistant search requests and read usage and spend, with the access of whoever connects."
      actions={
        settings ? (
          <Switch
            label="Admin MCP server"
            hideLabel
            checked={on}
            disabled={!canEdit || busy}
            disabledReason="You don't have permission to change MCP settings."
            onChange={(next) => void toggle(next)}
          />
        ) : null
      }
    >
      {loadError ? <ErrorState error={loadError} subject="MCP settings" size="sm" /> : null}
      <InlineError error={error} />
      {settings ? (
        <Facts
          items={[
            {
              label: 'Address',
              value: on ? (
                <span className="flex items-center gap-1">
                  <span className="mono break-all">{address}</span>
                  <CopyButton value={address} label="Copy the MCP server address" />
                </span>
              ) : (
                <span className="text-muted">Off</span>
              ),
              hint: on ? 'Connect with an admin token, or a signed-in session.' : undefined,
            },
            {
              label: 'Tools',
              value: 'Search requests, open a request, usage summary, spend summary',
              hint: 'Read-only. Each tool needs the same permission as the matching page.',
            },
          ]}
        />
      ) : null}
    </Card>
  );
}
