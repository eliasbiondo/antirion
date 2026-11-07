'use client';

import type { ReactNode } from 'react';

import type { IconName } from '@/components/ui/icon';
import { translator } from '@/lib/i18n';

import { ApiEndpointsSection } from './api-endpoints-section';
import { AuditSection } from './audit-section';
import { ConfigSection } from './config-section';
import { HealthSection } from './health-section';
import { NodeSection } from './node-section';
import { MetricsCard } from './metrics-card';
import { ObjectivesSection } from './objectives-section';
import { AdminMcpCard } from './admin-mcp-card';
import { DangerSection } from './danger-section';
import { McpSessionsCard } from './mcp-sessions-card';
import { EncryptionCard } from './encryption-card';
import { AuditSinksSection } from './audit-sinks-section';
import { ConnectionsSection } from './connections-section';
import { NotificationsMatrixSection } from './notification-matrix';
import { AlertTimingCard } from './observability-extras';
import { SmtpCard } from './smtp-card';
import { WebhooksSection } from './webhooks-section';
import { ApiAccessSettings } from './api-access-section';
import { IdentityProvidersSection } from './identity-providers';
import { ApiAccessSection, GeneralSection, ProvisioningSection } from './org-sections';
import { SignInSection } from './sign-in-section';
import { SignInExtras } from './sign-in-extras';
import { PrivacySection } from './privacy-section';
import { TRAFFIC_SECTIONS } from './traffic-sections';
import { TransportCard } from './transport-card';

export type SectionScope = 'gateway' | 'organization' | 'mixed' | 'follows';

export interface Section {
  id: string;
  label: string;
  icon: IconName;
  /** The page's one-line purpose, under its title. */
  description?: string;
  /** Pages with tables or an editor use the full width; forms stay narrow. */
  wide?: boolean;
  /** Older section ids that now open this page (links keep working). */
  aliases?: string[];
  /** Listed only for principals holding this permission (or one of these): the page reads nothing without it. */
  permission?: string | readonly string[];
  /**
   * Whom the page's settings are for: `organization`, one organization (the default one at gateway
   * scope); `gateway`, the whole gateway; `mixed`, gateway-wide settings with some for the
   * organization; `follows`, the whole gateway at gateway scope, else one organization plus some
   * gateway-wide settings. Gateway pages are `gateway` when unset.
   */
  scope?: SectionScope;
  render: () => ReactNode;
}

/**
 * Settings in three groups by who owns them: the organization, its integrations, and the gateway
 * itself; thin pages of the old layout are merged into the page they belong to. Providers has its
 * own page.
 */
export const SETTINGS_GROUPS: { label: string; sections: Section[] }[] = [
  {
    label: 'Organization',
    sections: [
      {
        id: 'general',
        scope: 'organization',
        permission: 'organizations:read',
        label: 'General',
        icon: 'settings',
        description: 'Name, logo and default time zone.',
        render: () => <GeneralSection />,
      },
      {
        id: 'sign-in',
        scope: 'mixed',
        permission: ['security:read', 'organizations:read'],
        label: 'Sign-in & SSO',
        icon: 'lock',
        description: 'How people sign in to this console, and how their accounts are kept in sync.',
        aliases: ['sessions', 'identity', 'provisioning'],
        render: () => (
          <>
            <SignInSection />
            <IdentityProvidersSection />
            <ProvisioningSection />
            <SignInExtras />
          </>
        ),
      },
      {
        id: 'api-access',
        scope: 'mixed',
        permission: 'organizations:read',
        label: 'API access',
        icon: 'key',
        description: 'Where apps may call from, their default limits and how long their keys live.',
        render: () => <ApiAccessSettings />,
      },
      {
        id: 'privacy',
        scope: 'follows',
        permission: ['policies:read', 'config:read'],
        label: 'Data & privacy',
        icon: 'list',
        description: 'What the request log keeps, for how long, and what is removed before it is stored.',
        render: () => <PrivacySection />,
      },
    ],
  },
  {
    label: 'Integrations',
    sections: [
      {
        id: 'notifications',
        scope: 'gateway',
        permission: ['alerts:read', 'config:read'],
        label: 'Notifications',
        icon: 'bell',
        description: 'Where alerts and events are sent, and the connections the gateway uses.',
        aliases: ['connections', 'email'],
        wide: true,
        render: () => (
          <>
            <ConnectionsSection />
            <NotificationsMatrixSection />
            <AlertTimingCard />
            <SmtpCard />
          </>
        ),
      },
      {
        id: 'webhooks',
        scope: 'gateway',
        permission: ['config:read', 'security:read'],
        label: 'Webhooks & audit copies',
        icon: 'webhook',
        description: 'Send gateway events to your own endpoints, and copy the audit log to external storage.',
        aliases: ['audit-sinks'],
        wide: true,
        render: () => (
          <>
            <WebhooksSection />
            <AuditSinksSection />
          </>
        ),
      },
    ],
  },
  {
    label: 'Gateway',
    sections: [
      {
        id: 'status',
        permission: 'health:read',
        label: 'Status',
        icon: 'heart',
        description:
          'How the gateway, its nodes, its deployments and its objectives are doing right now, and where its metrics are.',
        wide: true,
        aliases: ['health'],
        render: () => (
          <>
            <HealthSection />
            <ObjectivesSection />
            <MetricsCard />
          </>
        ),
      },
      {
        id: 'gateway',
        permission: ['policies:read', 'security:read'],
        label: 'Endpoints & security',
        icon: 'globe',
        description:
          'Where apps send requests, rate limiting, network and browser access, request sizes, encryption and MCP.',
        aliases: ['encryption'],
        render: () => (
          <>
            <ApiAccessSection />
            <EncryptionCard />
            <TransportCard />
            <AdminMcpCard />
            <McpSessionsCard />
          </>
        ),
      },
      {
        id: 'api-endpoints',
        permission: 'config:read',
        label: 'API & endpoints',
        icon: 'plug',
        description: 'Which endpoints apps may call, how requests and responses are handled, and streaming.',
        render: () => <ApiEndpointsSection />,
      },
      {
        id: 'node',
        permission: 'config:read',
        label: 'Node',
        icon: 'server',
        description:
          'Listeners, the console, the store and the cluster, as the configuration file sets them.',
        render: () => <NodeSection />,
      },
      {
        id: 'configuration',
        permission: 'config:read',
        label: 'Configuration',
        icon: 'file',
        description: 'The whole configuration as one document: edit, review the changes, then apply.',
        wide: true,
        render: () => <ConfigSection />,
      },
      {
        id: 'audit',
        permission: 'audit:read',
        label: 'Audit log',
        icon: 'clock',
        description: 'Every change and denied action, newest first.',
        wide: true,
        render: () => <AuditSection />,
      },
    ],
  },
  { label: 'Traffic', sections: TRAFFIC_SECTIONS },
  {
    label: '',
    sections: [
      {
        id: 'danger',
        permission: ['config:apply', 'models:write', 'cache:admin', 'security:write', 'organizations:delete'],
        label: 'Danger zone',
        icon: 'alert',
        description: 'Actions that affect every app and person using the gateway.',
        render: () => <DangerSection />,
      },
    ],
  },
];

export const SETTINGS_SECTIONS = SETTINGS_GROUPS.flatMap((g) => g.sections);

const GENERAL: Section = SETTINGS_GROUPS[0]?.sections[0] ?? {
  id: 'general',
  label: 'General',
  icon: 'settings',
  render: () => <GeneralSection />,
};

/**
 * The page for a section id, old ids included; General when there is none. With `allowed`, a
 * section the principal may not read gives way to the first one it may (null when there is none).
 */
export function findSection(id: string | null): Section;
export function findSection(id: string | null, allowed: (s: Section) => boolean): Section | null;
export function findSection(id: string | null, allowed?: (s: Section) => boolean): Section | null {
  const named = id
    ? (SETTINGS_SECTIONS.find((s) => s.id === id || s.aliases?.includes(id)) ?? GENERAL)
    : GENERAL;
  if (!allowed || allowed(named)) return named;
  return SETTINGS_SECTIONS.find(allowed) ?? null;
}

/** Whether the principal may read any Settings section, so the account menu offers Gateway settings. */
export function anySection(allowed: (s: Section) => boolean): boolean {
  return SETTINGS_SECTIONS.some(allowed);
}

/** Section ids that moved out of Settings to a page of their own. */
export const MOVED_SECTIONS: Record<string, string> = { providers: '/providers/' };

/**
 * Whom the open section's settings are for, under the dialog's title, from the section's `scope`
 * and the scope picker: one organization (the default one while the scope is the whole gateway),
 * the whole gateway, or both.
 */
export function settingsScopeWords(
  sectionId: string,
  scope: { kind: string; name?: string },
  translate = translator('en'),
): string {
  const section = SETTINGS_SECTIONS.find((s) => s.id === sectionId);
  const org =
    scope.kind === 'gateway'
      ? translate('the default organization')
      : (scope.name ?? translate('this organization'));
  switch (section?.scope ?? 'gateway') {
    case 'organization':
      return translate('For {0}', org);
    case 'mixed':
      return translate('For the whole gateway and {0}', org);
    case 'follows':
      return scope.kind === 'gateway'
        ? translate('For the whole gateway')
        : translate('For {0} and the whole gateway', org);
    default:
      return translate('For the whole gateway');
  }
}
