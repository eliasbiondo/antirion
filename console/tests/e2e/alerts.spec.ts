import http from 'node:http';
import type { AddressInfo } from 'node:net';

import { admin, expect, pickOption, test, unique } from './fixtures';

/** A receiver for webhook deliveries that keeps the bodies it got. */
async function receiver(): Promise<{ url: string; bodies: string[]; close: () => Promise<void> }> {
  const bodies: string[] = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk: Buffer) => (body += chunk.toString()));
    req.on('end', () => {
      bodies.push(body);
      res.writeHead(200).end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}/hook`,
    bodies,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

test('an event webhook and its channel, a rule from a template, edited, turned off and deleted', async ({
  signedIn: page,
  request,
}) => {
  test.setTimeout(60_000);
  const sink = await receiver();
  const hook = unique('hook');
  const channel = unique('chan');
  const rule = unique('rule');
  try {
    await page.goto('alerts/?view=channels');

    // An event webhook subscribed to the deployment events, tested.
    await page.getByRole('button', { name: 'New webhook' }).click();
    const hookDrawer = page.getByRole('dialog', { name: 'New event webhook' });
    await hookDrawer.getByRole('textbox', { name: /Name/ }).fill(hook);
    await hookDrawer.getByRole('textbox', { name: /Endpoint URL/ }).fill(sink.url);
    await hookDrawer.getByRole('checkbox', { name: /Deployments/ }).click();
    await hookDrawer.getByRole('button', { name: 'Add webhook' }).click();
    await expect(hookDrawer).toBeHidden();
    const hooks = page.getByRole('table', { name: 'Event webhooks' });
    const hookRow = hooks.getByRole('row').filter({ hasText: hook });
    await expect(hookRow.getByText('deployment.state_changed')).toBeVisible();
    await hookRow.getByRole('button', { name: 'Test' }).click();
    await expect(page.getByText('Test delivered')).toBeVisible();
    await expect.poll(() => sink.bodies.some((b) => b.includes('test.ping'))).toBe(true);

    // A channel that sends alerts through it.
    await page.getByRole('button', { name: 'New channel' }).click();
    const chanDrawer = page.getByRole('dialog', { name: 'New notification channel' });
    await chanDrawer.getByRole('textbox', { name: /Name/ }).fill(channel);
    await chanDrawer.getByRole('radio', { name: 'Webhook' }).click();
    await pickOption(
      page,
      chanDrawer.getByRole('combobox', { name: /Event webhook/ }),
      new RegExp(`^${hook}`),
    );
    await chanDrawer.getByRole('button', { name: 'Add channel' }).click();
    await expect(chanDrawer).toBeHidden();
    const channels = page.getByRole('table', { name: 'Notification channels' });
    await expect(channels.getByText(channel, { exact: true })).toBeVisible();

    // A rule from the "Provider down" template that notifies the channel.
    await page.getByRole('tab', { name: /Rules/ }).click();
    await page.getByRole('button', { name: 'New rule' }).first().click();
    const gallery = page.getByRole('dialog', { name: 'New alert rule' });
    await gallery.getByRole('button', { name: /^Provider down/ }).click();
    const editor = page.getByRole('dialog', { name: 'New rule: Provider down' });
    await expect(editor.getByRole('radio', { name: 'An event happens' })).toBeChecked();
    await editor.getByRole('combobox', { name: /^Notify/ }).click();
    await page
      .getByRole('listbox')
      .getByRole('option', { name: new RegExp(channel) })
      .click();
    await page.keyboard.press('Escape');
    await editor.getByRole('textbox', { name: /Rule name/ }).fill(rule);
    await editor.getByRole('button', { name: 'Create rule' }).click();
    await expect(editor).toBeHidden();
    await expect(page.getByRole('button', { name: rule, exact: true })).toBeVisible();
    const created = (await (await admin(request, 'GET', `/alerts/rules/${rule}`)).json()) as {
      spec: Record<string, unknown>;
    };
    expect(created.spec).toMatchObject({
      kind: 'event',
      events: ['deployment.state_changed'],
      channels: [channel],
      severity: 'critical',
    });

    // Silence it during maintenance windows.
    await page.getByRole('button', { name: rule, exact: true }).click();
    const edit = page.getByRole('dialog', { name: rule });
    await edit.getByRole('switch', { name: 'Silence during maintenance windows' }).click();
    await edit.getByRole('button', { name: 'Save changes' }).click();
    await expect(edit).toBeHidden();
    await expect
      .poll(async () => {
        const r = (await (await admin(request, 'GET', `/alerts/rules/${rule}`)).json()) as {
          spec: { silence_during_maintenance?: boolean };
        };
        return r.spec.silence_during_maintenance;
      })
      .toBe(true);

    // Turn it off from the manage dialog, found by search.
    await page.getByRole('button', { name: 'Manage rules' }).click();
    const manage = page.getByRole('dialog', { name: 'Manage alert rules' });
    await manage.getByRole('textbox', { name: 'Search rules' }).fill(rule);
    await manage.getByRole('switch', { name: `Turn off ${rule}` }).click();
    await expect(manage.getByRole('switch', { name: `Turn on ${rule}` })).toBeVisible();
    await manage.getByRole('button', { name: 'Done' }).click();

    // Delete the rule, then the channel and the webhook.
    await page.getByRole('button', { name: `Actions for ${rule}` }).click();
    await page.getByRole('menuitem', { name: 'Delete' }).click();
    const deleteRule = page.getByRole('alertdialog');
    await deleteRule.getByLabel(/to confirm/).fill(rule);
    await deleteRule.getByRole('button', { name: 'Delete rule' }).click();
    await expect(page.getByRole('button', { name: rule, exact: true })).toHaveCount(0);

    await page.getByRole('tab', { name: /Channels/ }).click();
    await page.getByRole('button', { name: `Actions for ${channel}` }).click();
    await page.getByRole('menuitem', { name: 'Delete' }).click();
    await page.getByRole('alertdialog').getByRole('textbox').fill(channel);
    await page.getByRole('alertdialog').getByRole('button', { name: 'Delete channel' }).click();
    await expect(page.getByRole('table', { name: 'Notification channels' }).getByText(channel)).toHaveCount(
      0,
    );
    await page.getByRole('button', { name: `Actions for ${hook}` }).click();
    await page.getByRole('menuitem', { name: 'Delete' }).click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Delete webhook' }).click();
    await expect(page.getByText(hook, { exact: true })).toHaveCount(0);
    expect((await admin(request, 'GET', `/alerts/rules/${rule}`)).status()).toBe(404);
  } finally {
    await sink.close();
  }
});
