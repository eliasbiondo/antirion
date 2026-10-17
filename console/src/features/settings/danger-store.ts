import { create } from 'zustand';

import type { OrganizationResource, Status } from '@/lib/api/types';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';
import { useShell } from '@/stores/shell';

import { type ChangeRequest, type GovernanceSettings, type Job, settingsApi } from './api';

export type DualAction = NonNullable<
  NonNullable<GovernanceSettings['spec']['dual_control']>['actions']
>[number];

export type MaintenanceWindow = NonNullable<GovernanceSettings['spec']['maintenance_windows']>[number];

interface DangerState {
  status: Slot<Status>;
  governance: Slot<GovernanceSettings>;
  organizations: Slot<OrganizationResource[]>;
  /** The organization the export and deletion cards act on, with its pending deletion. */
  target: Slot<OrganizationResource>;
  pending: Slot<ChangeRequest[]>;
  /** The export started in this browser session, followed until it ends. */
  job: Job | null;
  jobOrganization: string | null;
  load: () => Promise<void>;
  loadStatus: () => Promise<void>;
  loadPending: () => Promise<void>;
  loadTarget: (name: string) => Promise<void>;
  saveGovernance: (spec: Record<string, unknown>, reason?: string) => Promise<void>;
  setDryRun: (on: boolean, reason?: string) => Promise<void>;
  startExport: (organization: string) => Promise<void>;
  pollJob: () => Promise<void>;
  clearJob: () => void;
}

/** The danger zone's state: gateway status, governance, the target organization and held actions. */
export const useDanger = create<DangerState>()((set, get) => ({
  status: emptySlot(),
  governance: emptySlot(),
  organizations: emptySlot(),
  target: emptySlot(),
  pending: emptySlot(),
  job: null,
  jobOrganization: null,

  async load() {
    await Promise.all([
      get().loadStatus(),
      loadSlot(set, get, 'governance', settingsApi.governance),
      loadSlot(set, get, 'organizations', settingsApi.organizations),
      get().loadPending(),
    ]);
  },

  async loadStatus() {
    await loadSlot(set, get, 'status', settingsApi.status, { quiet: true });
    const status = get().status.data;
    if (status) useShell.setState({ status });
  },

  loadPending: () => loadSlot(set, get, 'pending', settingsApi.pendingActions, { quiet: true }),

  loadTarget: (name) => loadSlot(set, get, 'target', () => settingsApi.organization(name)),

  async saveGovernance(spec, reason) {
    await settingsApi.patchGovernance(spec, get().governance.data?.metadata.etag, reason);
    await loadSlot(set, get, 'governance', settingsApi.governance, { quiet: true });
    await get().loadStatus();
  },

  async setDryRun(on, reason) {
    const routing = await settingsApi.routing();
    await settingsApi.patchRouting({ dry_run_all: on }, routing.metadata.etag, reason);
    await get().loadStatus();
  },

  async startExport(organization) {
    const job = await settingsApi.exportOrganization(organization);
    set({ job, jobOrganization: organization });
  },

  async pollJob() {
    const job = get().job;
    if (!job) return;
    set({ job: await settingsApi.job(job.id) });
  },

  clearJob: () => set({ job: null, jobOrganization: null }),
}));

/** What a held action does, in words, from the call its change request carries. */
export function heldActionLabel(request: ChangeRequest): string {
  const change = request.changes[0];
  const path = (change?.path ?? '').split('?')[0] ?? '';
  const body = (change?.body ?? {}) as Record<string, unknown>;
  if (path.endsWith('/cache:invalidate'))
    return body.level === 'gateway' ? 'Clear the whole cache' : 'Clear part of the cache';
  if (path.endsWith('/runtime/ingress:pause'))
    return typeof body.organization === 'string'
      ? `Pause traffic of ${body.organization}`
      : 'Pause all traffic';
  const deletion = /\/organizations\/([^/:]+):schedule_deletion$/.exec(path);
  if (deletion) return `Delete organization ${decodeURIComponent(deletion[1] ?? '')}`;
  if (path.endsWith('/settings/security:rotate_secrets')) return 'Rotate gateway secrets';
  if (path.endsWith(':reset')) return 'Reset a budget';
  if (path.endsWith('/settings/governance')) return 'Remove an audit log destination';
  return request.title;
}
