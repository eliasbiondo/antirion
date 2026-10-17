import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';

import { navigation } from '@/test/setup';
import { signInAs } from '@/test/utils';

import { SettingsDialog } from '../components/settings-dialog';
import { useUnsaved } from '../unsaved';

function Harness() {
  return (
    <>
      <button type="button">Opener</button>
      <SettingsDialog />
    </>
  );
}

function at(search: string) {
  window.history.replaceState(null, '', `/${search}`);
}

describe('Settings dialog', () => {
  beforeEach(() => {
    signInAs([
      'config:read',
      'audit:read',
      'security:read',
      'organizations:read',
      'health:read',
      'config:apply',
    ]);
    useUnsaved.setState({ dirty: {} });
    navigation.replace.mockClear();
    at('');
  });

  it('opens at the section in the URL, over the page, and closes on Escape with focus restored', async () => {
    const { rerender } = render(<Harness />);
    expect(screen.queryByRole('dialog', { name: 'Gateway settings' })).not.toBeInTheDocument();
    screen.getByRole('button', { name: 'Opener' }).focus();

    act(() => at('?org=gateway&settings=audit'));
    rerender(<Harness />);
    const dialog = screen.getByRole('dialog', { name: 'Gateway settings' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(within(dialog).getByRole('heading', { level: 3, name: 'Audit log' })).toBeInTheDocument();
    const current = within(dialog).getByRole('button', { name: 'Audit log' });
    expect(current).toHaveAttribute('aria-current', 'page');
    expect(current).toHaveFocus();

    await userEvent.keyboard('{Escape}');
    expect(navigation.replace).toHaveBeenLastCalledWith('/?org=gateway', { scroll: false });
    rerender(<Harness />);
    expect(screen.queryByRole('dialog', { name: 'Gateway settings' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Opener' })).toHaveFocus();
  });

  it('keeps old section ids and switches sections from the list', async () => {
    at('?settings=health');
    render(<Harness />);
    const dialog = screen.getByRole('dialog', { name: 'Gateway settings' });
    expect(within(dialog).getByRole('heading', { level: 3, name: 'Status' })).toBeInTheDocument();
    expect(within(dialog).queryByRole('button', { name: 'Providers' })).not.toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: 'Danger zone' }));
    expect(navigation.replace).toHaveBeenLastCalledWith('/?settings=danger', { scroll: false });
  });

  it('asks before closing over unsaved changes, with our own confirmation', async () => {
    at('?settings=danger');
    render(<Harness />);
    act(() => useUnsaved.getState().mark('form', true));
    expect(screen.getByText('Unsaved changes')).toBeInTheDocument();

    await userEvent.click(screen.getAllByRole('button', { name: 'Close settings' })[0] ?? document.body);
    const confirm = screen.getByRole('alertdialog', { name: 'Discard changes?' });
    expect(navigation.replace).not.toHaveBeenCalled();

    // Escape closes the confirmation only; Settings stays open.
    await userEvent.keyboard('{Escape}');
    expect(confirm).not.toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'Gateway settings' })).toBeInTheDocument();
    expect(navigation.replace).not.toHaveBeenCalled();

    await userEvent.keyboard('{Escape}');
    await userEvent.click(
      within(screen.getByRole('alertdialog', { name: 'Discard changes?' })).getByRole('button', {
        name: 'Discard',
      }),
    );
    expect(navigation.replace).toHaveBeenLastCalledWith('/', { scroll: false });
  });

  it('sends an old Providers link to the Providers page', () => {
    at('?org=gateway&settings=providers');
    render(<Harness />);
    expect(screen.queryByRole('dialog', { name: 'Gateway settings' })).not.toBeInTheDocument();
    expect(navigation.replace).toHaveBeenLastCalledWith('/providers/?org=gateway');
  });
});
