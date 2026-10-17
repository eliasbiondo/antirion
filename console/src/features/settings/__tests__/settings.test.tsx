import { render, screen } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { emptySlot } from '@/lib/slot';
import { signInAs } from '@/test/utils';

import { actionLabel } from '../components/audit-section';
import { ApiAccessSection } from '../components/org-sections';
import { findSection, MOVED_SECTIONS } from '../components/sections';
import { useOrgSettings } from '../store';

describe('settings sections', () => {
  it('groups the old pages and keeps their links working', () => {
    expect(findSection('sessions').id).toBe('sign-in');
    expect(findSection('identity').id).toBe('sign-in');
    expect(findSection('provisioning').id).toBe('sign-in');
    expect(findSection('connections').id).toBe('notifications');
    expect(findSection('audit-sinks').id).toBe('webhooks');
    expect(findSection('api-access').id).toBe('api-access');
    expect(findSection('encryption').id).toBe('gateway');
    expect(findSection('health').id).toBe('status');
    expect(MOVED_SECTIONS.providers).toBe('/providers/');
    expect(findSection(null).id).toBe('general');
    expect(findSection('nonsense').id).toBe('general');
  });

  it('words audit actions instead of methods and paths', () => {
    expect(actionLabel('GET /admin/v1/auth/me/preferences')).toBe('Read preferences');
    expect(actionLabel('keys.create')).toBe('Key created');
    expect(actionLabel('budgets.adjust')).toBe('Budget adjusted');
    expect(actionLabel('POST /admin/v1/keys/{name}:rotate')).toBe('Rotate keys');
  });

  it('shows the gateway settings as facts, never raw {} or a config path', async () => {
    signInAs(['config:read', 'security:read']);
    useOrgSettings.setState({
      limits: emptySlot(),
      security: emptySlot(),
      auth: emptySlot(),
      observability: emptySlot(),
    });
    const empty = () => HttpResponse.json({ spec: {}, metadata: { etag: '"1"', generation: 1 } });
    server.use(
      http.get('*/admin/v1/settings/limits', empty),
      http.get('*/admin/v1/settings/security', empty),
      http.get('*/admin/v1/settings/auth', empty),
      http.get('*/admin/v1/settings/observability', empty),
    );
    const { container } = render(<ApiAccessSection />);
    expect(await screen.findByText('Calls to private networks')).toBeInTheDocument();
    expect(screen.getByText('Not set')).toBeInTheDocument();
    expect(container.textContent).not.toContain('{}');
    expect(container.textContent).not.toContain('server.public_url');
  });
});
