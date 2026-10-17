import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, test, TOKEN, unique } from './fixtures';

async function call(request: APIRequestContext, method: string, path: string, body?: unknown) {
  const response = await request.fetch(`${ADMIN_URL}/admin/v1${path}`, {
    method,
    headers: {
      authorization: `Bearer ${TOKEN}`,
      'content-type': method === 'PATCH' ? 'application/merge-patch+json' : 'application/json',
    },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(response.status(), `${method} ${path}: ${await response.text()}`).toBeLessThan(300);
  return response;
}

test('a custom role created, edited and deleted; a user with their sessions, permissions and activity; seats', async ({
  signedIn: page,
  request,
}) => {
  const role = unique('fin').replace(/-/g, '_');
  const user = unique('dir');
  await call(request, 'POST', '/users', {
    name: user,
    spec: {
      kind: 'human',
      display_name: 'Directory Person',
      email: `${user}@example.com`,
      role_bindings: [{ role: 'team_admin', scope: 'team/default' }],
    },
  });
  const token = (await (await call(request, 'POST', `/users/${user}/tokens`, {})).json()) as {
    token: string;
  };
  const session = await request.fetch(`${ADMIN_URL}/admin/v1/auth/session`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0',
    },
    data: { token: token.token },
  });
  expect(session.status()).toBe(200);

  await page.goto('teams/?tab=roles');
  await page.getByRole('button', { name: 'Create role' }).click();
  const drawer = page.getByRole('dialog', { name: 'Create role' });
  await drawer.getByLabel(/^Name/).fill(role);
  await drawer.getByLabel('Description').fill('Reads spend');
  await drawer.getByRole('checkbox', { name: 'Analytics: see' }).click();
  await drawer.getByRole('button', { name: 'Create role' }).click();
  await expect(drawer).toBeHidden();
  const label = role.charAt(0).toUpperCase() + role.slice(1).replace(/_/g, ' ');
  const row = page.getByRole('row', { name: new RegExp(label, 'i') });
  await expect(row).toContainText('Custom');
  await expect(row).toContainText('Analytics: see');

  await row.getByRole('button', { name: `Actions for ${label}` }).click();
  await page.getByRole('menuitem', { name: 'Edit' }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${label}` });
  await edit.getByRole('checkbox', { name: 'Teams: see' }).click();
  await edit.getByRole('button', { name: 'Save changes' }).click();
  await expect(edit).toBeHidden();
  const roles = (await (await call(request, 'GET', '/roles')).json()) as {
    items: { name: string; permissions: string[] }[];
  };
  expect(roles.items.find((r) => r.name === role)?.permissions.sort()).toEqual([
    'analytics:read',
    'teams:read',
  ]);

  await row.getByRole('button', { name: `Actions for ${label}` }).click();
  await page.getByRole('menuitem', { name: 'Delete' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Delete role' }).click();
  await expect(page.getByRole('row', { name: new RegExp(label, 'i') })).toHaveCount(0);

  await page.goto(`teams/?user=${user}`);
  const sessions = page.getByRole('table', { name: `Sessions of ${user}` });
  await expect(sessions).toContainText('Firefox on Linux');
  await expect(sessions).toContainText('Admin token');
  await expect(page.getByText('Admin token', { exact: true }).first()).toBeVisible();
  await expect(page.getByText('No two-step verification')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'What they can do' })).toBeVisible();
  await expect(page.getByText('as Team admin')).toBeVisible();
  await page.getByRole('button', { name: 'Sign out everywhere' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Sign out everywhere' }).click();
  await expect(page.getByText('Not signed in anywhere.')).toBeVisible();

  await page.goto('teams/?tab=activity');
  await page.getByLabel('Search activity').fill(user);
  await expect(page.getByRole('list', { name: 'Directory activity' })).toContainText('Added');
  await expect(page.getByRole('list', { name: 'Directory activity' })).toContainText('Signed out everywhere');

  const status = (await (await call(request, 'GET', '/status')).json()) as { users: { active: number } };
  await call(request, 'PATCH', '/settings/auth', {
    spec: { admin: { max_active_users: status.users.active } },
  });
  try {
    await page.goto('teams/');
    await expect(page.getByText(`All ${status.users.active} seats in use`)).toBeVisible();
    await expect(page.getByRole('button', { name: 'Invite people' })).toBeDisabled();
  } finally {
    await call(request, 'PATCH', '/settings/auth', { spec: { admin: { max_active_users: null } } });
    await call(request, 'DELETE', `/users/${user}`);
  }
});

test("an organization's details, and deleting it with everything in it", async ({
  signedIn: page,
  request,
}) => {
  const org = unique('org');
  const team = unique('t');
  const project = unique('p');
  const key = unique('k');
  await call(request, 'POST', '/organizations', { name: org, spec: { display_name: 'E2E Org' } });
  await call(request, 'POST', '/teams', { name: team, spec: { organization: org } });
  await call(request, 'POST', '/projects', { name: project, spec: { team } });
  await call(request, 'POST', '/keys', { name: key, spec: { project } });

  await page.goto(`teams/?tab=organizations&detail=${org}`);
  const drawer = page.getByRole('dialog', { name: 'Organization E2E Org' });
  await expect(drawer).toContainText(key);
  await expect(drawer).toContainText('Its 1 team, 1 project and 1 key are deleted with it');
  await expect(drawer).toContainText('Apps using its 1 key are refused');
  await drawer.getByRole('button', { name: 'Delete' }).click();
  const confirm = page.getByRole('alertdialog', { name: `Delete organization ${org}?` });
  await expect(confirm).toContainText('Everything in it is deleted too: 1 team, 1 project and 1 key');
  await confirm.getByRole('textbox').first().fill(org);
  await confirm.getByRole('button', { name: 'Delete organization' }).click();
  await expect(drawer).toBeHidden();
  for (const path of [`/organizations/${org}`, `/teams/${team}`, `/projects/${project}`, `/keys/${key}`]) {
    const r = await request.fetch(`${ADMIN_URL}/admin/v1${path}`, {
      headers: { authorization: `Bearer ${TOKEN}` },
    });
    expect(r.status(), path).toBe(404);
  }
});
