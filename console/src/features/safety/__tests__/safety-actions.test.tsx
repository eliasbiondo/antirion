import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { pickOption, signInAs } from '@/test/utils';

import { SafetyScreen } from '../components/safety-screen';
import { useSafety } from '../store';

const WRITER = ['logs:read', 'policies:read', 'policies:write', 'safety:review', 'notifications:send'];

function reset(view = '') {
  useSafety.setState({
    events: emptySlot(),
    guardrails: emptySlot(),
    runtime: emptySlot(),
    topics: emptySlot(),
    policies: emptySlot(),
    byCategory: emptySlot(),
    byEntity: emptySlot(),
    lastHour: emptySlot(),
    teams: emptySlot(),
    models: emptySlot(),
    posture: emptySlot(),
  });
  window.history.replaceState(null, '', `/safety/${view}`);
}

function policies(input: string[] = ['pii-gateway']) {
  let patched: unknown = null;
  let ifMatch: string | null = null;
  server.use(
    http.get('*/admin/v1/settings/policies', () =>
      HttpResponse.json({
        spec: { guardrails: { input, output: [] } },
        metadata: { etag: '"p1"', generation: 3, created_at: '', updated_at: '', managed_by: 'api' },
      }),
    ),
    http.patch('*/admin/v1/settings/policies', async ({ request }) => {
      patched = await request.json();
      ifMatch = request.headers.get('if-match');
      return HttpResponse.json({ spec: {}, metadata: { etag: '"p2"' } });
    }),
  );
  return { patched: () => patched, ifMatch: () => ifMatch };
}

describe('safety rules', () => {
  it('creates a pattern rule with a checksum and runs it on every prompt', async () => {
    reset('?view=rules');
    signInAs(WRITER);
    const pol = policies();
    let created: unknown = null;
    server.use(
      http.post('*/admin/v1/guardrails', async ({ request }) => {
        created = await request.json();
        return HttpResponse.json({}, { status: 201 });
      }),
    );
    render(<SafetyScreen />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'New rule' }));
    const drawer = await screen.findByRole('dialog', { name: 'New rule' });
    await user.type(within(drawer).getByLabelText('Name'), 'iban_numbers');
    await user.type(within(drawer).getByLabelText('Patterns'), 'GB\\d{{2}');
    await pickOption(within(drawer).getByLabelText('Checksum'), 'IBAN mod-97');
    await user.click(within(drawer).getByRole('button', { name: 'Create rule' }));
    await waitFor(() => expect(created).not.toBeNull());
    expect(created).toEqual({
      name: 'iban_numbers',
      spec: {
        type: 'regex',
        enabled: true,
        action: 'block',
        regex: { deny: ['GB\\d{2}'], validate: 'iban_mod97' },
      },
    });
    await waitFor(() =>
      expect(pol.patched()).toEqual({
        spec: { guardrails: { input: ['pii-gateway', 'iban_numbers'], output: [] } },
      }),
    );
    expect(pol.ifMatch()).toBe('"p1"');
  });

  it('offers topic restriction, rewrites, JSON checks, templates and affixes in the picker', async () => {
    reset('?view=rules');
    signInAs(WRITER);
    policies();
    render(<SafetyScreen />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'New rule' }));
    const drawer = await screen.findByRole('dialog', { name: 'New rule' });
    await user.click(within(drawer).getByLabelText('Method'));
    const list = await screen.findByRole('listbox');
    for (const name of [
      'Topic restriction',
      'Rewrite',
      'JSON schema',
      'JSON repair',
      'Prompt templates',
      'Prefix and suffix',
    ])
      expect(within(list).getByRole('option', { name })).toBeInTheDocument();
  });

  it('edits, tests and deletes a rule from the table', async () => {
    reset('?view=rules');
    signInAs(WRITER);
    policies(['pii-gateway', 'custom-guard']);
    let tested: unknown = null;
    let deleted = '';
    let patched: unknown = null;
    server.use(
      http.post(/\/guardrails\/custom-guard:test$/, async ({ request }) => {
        tested = await request.json();
        return HttpResponse.json({
          verdict: 'flag',
          category: 'deny_pattern',
          spans: [{ start: 8, end: 12, entity: 'REDACTED' }],
          redacted: null,
        });
      }),
      http.delete('*/admin/v1/guardrails/:name', ({ params }) => {
        deleted = String(params.name);
        return new HttpResponse(null, { status: 204 });
      }),
      http.patch('*/admin/v1/settings/policies', async ({ request }) => {
        patched = await request.json();
        return HttpResponse.json({ spec: {}, metadata: { etag: '"p2"' } });
      }),
    );
    render(<SafetyScreen />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'custom-guard' }));
    const drawer = await screen.findByRole('dialog', { name: 'custom-guard' });
    const test = within(drawer).getByRole('region', { name: 'Test this rule' });
    await user.type(within(test).getByLabelText('Sample text'), 'card is 4242 here');
    await user.click(within(test).getByRole('button', { name: 'Run test' }));
    expect(await within(test).findByRole('status')).toHaveTextContent('Flag only');
    expect(tested).toEqual({ stage: 'input', text: 'card is 4242 here' });
    await user.click(within(drawer).getByRole('button', { name: 'Delete rule' }));
    const confirm = await screen.findByRole('alertdialog', { name: 'Delete rule custom-guard?' });
    await user.type(within(confirm).getByRole('textbox'), 'custom-guard');
    await user.click(within(confirm).getByRole('button', { name: 'Delete rule' }));
    await waitFor(() => expect(deleted).toBe('custom-guard'));
    expect(patched).toEqual({ spec: { guardrails: { input: ['pii-gateway'], output: [] } } });
  });
});

describe('event actions', () => {
  it('prepares "allow similar" as an edit of the guardrail, then saves it with its ETag', async () => {
    reset();
    signInAs(WRITER);
    policies();
    let put: unknown = null;
    let ifMatch: string | null = null;
    server.use(
      http.put('*/admin/v1/guardrails/:name', async ({ request }) => {
        put = await request.json();
        ifMatch = request.headers.get('if-match');
        return HttpResponse.json({});
      }),
    );
    render(<SafetyScreen />);
    const user = userEvent.setup();
    await screen.findByRole('listbox', { name: 'Safety events' });
    await user.click(screen.getByRole('button', { name: 'More' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Allow similar' }));
    const drawer = await screen.findByRole('dialog', { name: 'pii-gateway' });
    expect(drawer).toHaveTextContent('Stop checking for email address, phone number.');
    await user.click(within(drawer).getByRole('button', { name: 'Save rule' }));
    await waitFor(() => expect(put).not.toBeNull());
    expect(ifMatch).toBe('"g1"');
    expect(put).toEqual({
      spec: {
        type: 'pii',
        action: 'redact',
        enabled: true,
        pii: { entities: ['credit_card', 'us_ssn', 'iban', 'ip_address', 'ca_sin'] },
      },
    });
  });

  it('notifies the owning team with a link to the event', async () => {
    reset();
    signInAs(WRITER);
    const box: { sent: { to?: { team?: string }; subject?: string; link?: string } | null } = { sent: null };
    server.use(
      http.post(/\/notifications:send$/, async ({ request }) => {
        box.sent = (await request.json()) as typeof box.sent;
        return HttpResponse.json({ recipients: 3 });
      }),
    );
    render(<SafetyScreen />);
    const user = userEvent.setup();
    await screen.findByRole('listbox', { name: 'Safety events' });
    await user.click(screen.getByRole('button', { name: 'More' }));
    await user.click(await screen.findByRole('menuitem', { name: /^Notify / }));
    const dialog = await screen.findByRole('dialog', { name: /^Notify team / });
    await user.click(within(dialog).getByRole('button', { name: 'Send notification' }));
    await waitFor(() => expect(box.sent).not.toBeNull());
    expect(box.sent?.to?.team).toBeTruthy();
    expect(box.sent?.subject).toMatch(/^Safety event: pii-gateway found/);
    expect(box.sent?.link).toContain('/safety/');
  });

  it('explains the verdict, exports the events and filters them from a personal data card', async () => {
    reset();
    signInAs(WRITER);
    policies();
    const { rerender } = render(<SafetyScreen />);
    const user = userEvent.setup();
    expect(await screen.findByText('Gateway (prompts)')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Export CSV' })).toBeInTheDocument();
    // The test router changes the address without rendering again, as Next would.
    await user.click(screen.getByRole('tab', { name: /Personal data/ }));
    rerender(<SafetyScreen />);
    await user.click(await screen.findByRole('button', { name: /^Phone number/ }));
    rerender(<SafetyScreen />);
    expect(await screen.findByRole('button', { name: 'Stop filtering to Phone number' })).toBeInTheDocument();
    const list = screen.getByRole('listbox', { name: 'Safety events' });
    expect(within(list).queryByText(/secret-scanner/)).not.toBeInTheDocument();
  });
});
