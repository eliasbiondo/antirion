import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import type { ChangeRequest } from '../api';
import { AdminMcpCard } from '../components/admin-mcp-card';
import { AuditSection } from '../components/audit-section';
import { DangerSection } from '../components/danger-section';
import { HealthSection } from '../components/health-section';
import { utcInstant } from '../components/maintenance-card';
import { PrivacySection } from '../components/privacy-section';
import { heldActionLabel, useDanger } from '../danger-store';
import { compareDocuments } from '../doc-diff';
import { usePrivacy } from '../privacy-store';
import { useAudit, useOrgSettings, useRuntime } from '../store';

const meta = { etag: '"7"', generation: 7, created_at: '2026-10-01T00:00:00Z', managed_by: 'api' };
const list = (items: unknown[]) => HttpResponse.json({ items, next_cursor: null });

function held(path: string, body?: unknown): ChangeRequest {
  return {
    id: 'cr1',
    kind: 'action',
    title: 'Held',
    author: 'ada',
    created_at: '2026-10-15T10:00:00Z',
    changes: [{ method: 'POST', path, body }],
    state: 'open',
    approvals: [],
    approvals_required: 1,
  };
}

describe('configuration versions', () => {
  it('compares two documents per resource and per settings member, ignoring the startup blocks', () => {
    const running = {
      version: 1,
      server: { listen: ':8080' },
      providers: { a: { type: 'openai' }, b: { type: 'anthropic' } },
      governance: { dual_control: { actions: ['cache_flush_all'] } },
    };
    const old = { providers: { a: { type: 'openai' } }, routing: { dry_run_all: true } };
    expect(compareDocuments(running, old)).toEqual([
      { op: 'delete', path: 'governance', before: running.governance, after: null },
      { op: 'delete', path: 'providers.b', before: { type: 'anthropic' }, after: null },
      { op: 'create', path: 'routing', before: null, after: { dry_run_all: true } },
    ]);
  });
});

describe('danger zone helpers', () => {
  it('words the actions the two-person rule holds', () => {
    expect(heldActionLabel(held('/admin/v1/cache:invalidate', { level: 'gateway' }))).toBe(
      'Clear the whole cache',
    );
    expect(heldActionLabel(held('/organizations/acme:schedule_deletion', { grace: '1d' }))).toBe(
      'Delete organization acme',
    );
    expect(heldActionLabel(held('/runtime/ingress:pause', { organization: 'acme' }))).toBe(
      'Pause traffic of acme',
    );
  });

  it('reads a maintenance window on the UTC clock', () => {
    expect(utcInstant('2026-10-20', '02:30')).toBe('2026-10-20T02:30:00Z');
    expect(utcInstant('2026-10-20', '25:00')).toBeNull();
  });
});

describe('Data & privacy', () => {
  beforeEach(() => {
    signInAs(['config:read', 'config:apply', 'policies:read', 'policies:write', 'security:read']);
    usePrivacy.setState({
      policy: emptySlot(),
      governance: emptySlot(),
      guardrails: emptySlot(),
      zdr: emptySlot(),
    });
    useOrgSettings.setState({
      observability: emptySlot(),
      auth: emptySlot(),
      limits: emptySlot(),
      security: emptySlot(),
    });
  });

  it('writes content logging, zero data retention and redaction to the gateway policy', async () => {
    const patches: unknown[] = [];
    server.use(
      http.get('*/admin/v1/settings/policies', () =>
        HttpResponse.json({ spec: { observability: { content: { enabled: true } } }, metadata: meta }),
      ),
      http.patch('*/admin/v1/settings/policies', async ({ request }) => {
        patches.push(await request.json());
        return HttpResponse.json({ spec: {}, metadata: meta });
      }),
      http.get('*/admin/v1/settings/governance', () =>
        HttpResponse.json({
          spec: {
            change_control: {
              protected_paths: [{ path: 'observability.request_logs.sinks.store.retention', above: '90d' }],
            },
          },
          metadata: meta,
        }),
      ),
      http.get('*/admin/v1/settings/observability', () =>
        HttpResponse.json({
          spec: { request_logs: { sinks: { store: { retention: '30d' } } } },
          metadata: meta,
        }),
      ),
      http.get('*/admin/v1/guardrails', () => list([])),
      http.get('*/admin/v1/organizations', () => list([])),
      http.get('*/admin/v1/teams', () => list([])),
      http.get('*/admin/v1/projects', () =>
        list([
          {
            name: 'support',
            spec: { team: 't', policies: { observability: { zero_data_retention: true } } },
            metadata: meta,
          },
        ]),
      ),
      http.get('*/admin/v1/keys', () => list([])),
    );
    render(<PrivacySection />);
    await userEvent.click(await screen.findByRole('switch', { name: 'Remove personal data before storing' }));
    await userEvent.click(screen.getByRole('radio', { name: /Replace it with a hash/ }));
    expect(screen.getByText('support')).toBeInTheDocument();
    await userEvent.click(screen.getAllByRole('button', { name: 'Save changes' })[0] ?? document.body);
    await waitFor(() =>
      expect(patches[0]).toEqual({
        spec: {
          observability: {
            zero_data_retention: false,
            content: { enabled: true, redact_pii: true, on_pii: 'hash' },
          },
        },
      }),
    );

    // A retention past the protected limit says it goes to review.
    expect(screen.getByText('Longer than 90d needs a second person’s approval.')).toBeInTheDocument();
    const retention = screen.getByRole('textbox', { name: 'Keep request details for' });
    await userEvent.clear(retention);
    await userEvent.type(retention, '180d');
    expect(screen.getByText(/Saving opens a change request/)).toBeInTheDocument();
  });
});

describe('Audit log', () => {
  it('leaves reads out on the server, and asks for them when shown', async () => {
    signInAs(['audit:read']);
    useAudit.setState({ query: { kind: 'write' }, entries: emptySlot(), cursor: null });
    const seen: (string | null)[] = [];
    server.use(
      http.get('*/admin/v1/audit', ({ request }) => {
        seen.push(new URL(request.url).searchParams.get('kind'));
        return list([]);
      }),
    );
    render(<AuditSection />);
    await waitFor(() => expect(seen).toEqual(['write']));
    await userEvent.click(screen.getByRole('switch', { name: 'Show reads' }));
    await waitFor(() => expect(seen).toEqual(['write', null]));
  });
});

describe('Danger zone', () => {
  beforeEach(() => {
    signInAs([
      'config:read',
      'config:apply',
      'security:read',
      'security:write',
      'organizations:read',
      'organizations:delete',
      'cache:admin',
      'routing:write',
      'logs:read',
    ]);
    useDanger.setState({
      status: emptySlot(),
      governance: emptySlot(),
      organizations: emptySlot(),
      target: emptySlot(),
      pending: emptySlot(),
      job: null,
      jobOrganization: null,
    });
  });

  it('lists held actions, saves the two-person rule and reads a scheduled deletion', async () => {
    const patches: unknown[] = [];
    server.use(
      http.get('*/admin/v1/status', () =>
        HttpResponse.json({
          version: '0.1.0',
          commit: 'abc',
          mode: 'standalone',
          test_mode: true,
          config_generation: 7,
          dry_run_all: false,
          ingress_pauses: [],
          maintenance_window: {
            start: '2026-10-20T02:00:00Z',
            end: '2026-10-20T04:00:00Z',
            description: 'Upgrade',
            active: false,
          },
        }),
      ),
      http.get('*/admin/v1/settings/governance', () =>
        HttpResponse.json({
          spec: {
            dual_control: { actions: ['cache_flush_all'], approval_ttl: '1h' },
            maintenance_windows: [
              { start: '2026-10-20T02:00:00Z', end: '2026-10-20T04:00:00Z', description: 'Upgrade' },
            ],
          },
          metadata: meta,
        }),
      ),
      http.patch('*/admin/v1/settings/governance', async ({ request }) => {
        patches.push(await request.json());
        return HttpResponse.json({ spec: {}, metadata: meta });
      }),
      http.get('*/admin/v1/organizations', () => list([{ name: 'acme', spec: {}, metadata: meta }])),
      http.get('*/admin/v1/organizations/acme', () =>
        HttpResponse.json({
          name: 'acme',
          spec: { enabled: false },
          metadata: {
            ...meta,
            deletion: {
              state: 'scheduled',
              delete_at: '2026-11-14T10:00:00Z',
              scheduled_by: 'ada',
              error: null,
            },
          },
        }),
      ),
      http.get('*/admin/v1/change_requests', () =>
        list([held('/admin/v1/cache:invalidate', { level: 'gateway' })]),
      ),
    );
    render(<DangerSection />);
    const waiting = (await screen.findByText('Waiting for approval')).closest('section') ?? document.body;
    expect(within(waiting).getByText('Clear the whole cache')).toBeInTheDocument();
    expect(within(waiting).getByRole('link', { name: 'View request' })).toBeInTheDocument();
    const windows = await screen.findByRole('table', { name: 'Maintenance windows' });
    expect(within(windows).getByText('Upgrade')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('checkbox', { name: /Rotate gateway secrets/ }));
    const rule = screen.getByText('Two-person rule').closest('section') ?? document.body;
    await userEvent.click(within(rule).getByRole('button', { name: 'Save changes' }));
    await waitFor(() =>
      expect(patches[0]).toEqual({
        spec: { dual_control: { actions: ['cache_flush_all', 'secrets_rotate'], approval_ttl: '1h' } },
      }),
    );

    const card = screen.getByText('Export or delete an organization').closest('section') ?? document.body;
    await userEvent.click(within(card).getByRole('combobox', { name: 'Organization' }));
    await userEvent.click(await screen.findByRole('option', { name: 'acme' }));
    expect(await within(card).findByText('Deletion scheduled')).toBeInTheDocument();
    expect(within(card).getByRole('button', { name: 'Cancel deletion' })).toBeInTheDocument();
  });
});

describe('Status', () => {
  it('drains a node after its name is typed', async () => {
    signInAs(['health:read', 'node:drain']);
    useRuntime.setState({
      health: emptySlot(),
      deployments: emptySlot(),
      providerStatus: emptySlot(),
      nodes: emptySlot(),
    });
    const drained: string[] = [];
    server.use(
      http.get('*/admin/v1/runtime/nodes', () =>
        list([
          {
            node_id: 'n1',
            version: '0.1.0',
            config_version: 3,
            ready: true,
            draining: false,
            degraded: [],
            last_seen: '2026-10-15T10:00:00Z',
          },
          {
            node_id: 'n2',
            version: '0.1.0',
            config_version: 3,
            ready: true,
            draining: false,
            degraded: [],
            last_seen: '2026-10-15T10:00:00Z',
          },
        ]),
      ),
      http.post('*/admin/v1/runtime/nodes/:id', ({ params }) => {
        drained.push(String(params.id));
        return HttpResponse.json({ status: 'draining', in_flight: 2 }, { status: 202 });
      }),
    );
    render(<HealthSection />);
    const table = await screen.findByRole('table', { name: 'Nodes' });
    await userEvent.click(
      within(within(table).getByText('n2').closest('tr') ?? table).getByRole('button', { name: 'Drain' }),
    );
    const dialog = screen.getByRole('alertdialog', { name: 'Drain node n2?' });
    expect(within(dialog).getByText(/sends them to the other nodes/)).toBeInTheDocument();
    await userEvent.type(within(dialog).getByRole('textbox'), 'n2');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Drain node' }));
    await waitFor(() => expect(drained).toEqual(['n2:drain']));
  });
});

describe('Admin MCP server', () => {
  it('turns the built-in server on and shows its address', async () => {
    signInAs(['mcp:read', 'mcp:write']);
    let enabled = false;
    const patches: unknown[] = [];
    server.use(
      http.get('*/admin/v1/settings/mcp', () =>
        HttpResponse.json({ spec: { admin_server: { enabled } }, metadata: meta }),
      ),
      http.patch('*/admin/v1/settings/mcp', async ({ request }) => {
        patches.push(await request.json());
        enabled = true;
        return HttpResponse.json({ spec: { admin_server: { enabled } }, metadata: meta });
      }),
    );
    render(<AdminMcpCard />);
    await userEvent.click(await screen.findByRole('switch', { name: 'Admin MCP server' }));
    await waitFor(() => expect(patches).toEqual([{ spec: { admin_server: { enabled: true } } }]));
    expect(await screen.findByText(/\/admin\/mcp$/)).toBeInTheDocument();
  });
});
