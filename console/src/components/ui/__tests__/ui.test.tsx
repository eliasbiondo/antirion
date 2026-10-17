import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { signInAs } from '@/test/utils';

import { Button } from '../button';
import { ConfirmDialog } from '../confirm';
import { Segmented } from '../form';
import { Gate } from '../gate';
import { MarkTile, ModelMark, ProviderMark, ProviderName } from '../marks';
import { CancelButton, Dialog, Drawer } from '../overlay';
import { RevealOnceDialog } from '../reveal-once';

describe('ConfirmDialog', () => {
  it('needs the typed name and sends the reason', async () => {
    const onConfirm = vi.fn(() => Promise.resolve());
    render(
      <ConfirmDialog
        open
        onClose={() => undefined}
        title="Delete key"
        effect="It cannot be undone."
        confirmLabel="Delete"
        typeToConfirm="ci-bot"
        onConfirm={onConfirm}
      />,
    );
    const confirm = screen.getByRole('button', { name: 'Delete' });
    expect(confirm).toBeDisabled();
    await userEvent.type(screen.getByLabelText(/to confirm/), 'ci-bot');
    await userEvent.click(screen.getByRole('button', { name: 'Add a reason for the audit log' }));
    await userEvent.type(screen.getByLabelText(/^Reason/), 'leaked');
    await userEvent.click(confirm);
    expect(onConfirm).toHaveBeenCalledWith('leaked');
  });

  it('shows the API error and stays open when the action fails', async () => {
    render(
      <ConfirmDialog
        open
        onClose={() => undefined}
        title="Revoke"
        effect="x"
        confirmLabel="Revoke"
        withReason={false}
        onConfirm={() => Promise.reject(new Error('store unavailable'))}
      />,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    expect(await screen.findByText('store unavailable')).toBeInTheDocument();
  });
});

describe('RevealOnceDialog', () => {
  function Harness() {
    const [secret, setSecret] = useState<string | null>('sk-antirion-abc-secret');
    return (
      <>
        <RevealOnceDialog secret={secret} onClose={() => setSecret(null)} title="Key created" />
        <span data-testid="held">{secret ?? 'dropped'}</span>
      </>
    );
  }

  it('holds the secret until acknowledged, then drops it', async () => {
    render(<Harness />);
    expect(screen.getByRole('alertdialog', { name: 'Key created' })).toHaveTextContent(
      'sk-antirion-abc-secret',
    );
    const done = screen.getByRole('button', { name: 'Done' });
    expect(done).toBeDisabled();
    await userEvent.click(screen.getByRole('checkbox'));
    await userEvent.click(done);
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(screen.getByTestId('held')).toHaveTextContent('dropped');
  });
});

describe('Gate', () => {
  it('disables with the missing permission as the reason, or hides', () => {
    signInAs(['keys:read']);
    render(
      <>
        <Gate operation="createKey">
          <Button>Create key</Button>
        </Gate>
        <Gate operation="deleteKey" mode="hide">
          <Button>Delete key</Button>
        </Gate>
      </>,
    );
    expect(screen.getByRole('button', { name: 'Create key' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Create key' })).toHaveAccessibleDescription(
      "You don't have permission to create API keys.",
    );
    expect(screen.queryByRole('button', { name: 'Delete key' })).not.toBeInTheDocument();
  });

  it('lets a permitted principal through', () => {
    signInAs(['keys:create']);
    render(
      <Gate operation="createKey">
        <Button>Create key</Button>
      </Gate>,
    );
    expect(screen.getByRole('button', { name: 'Create key' })).toBeEnabled();
  });
});

describe('Segmented', () => {
  it('is a radio group that moves with the arrow keys', async () => {
    function Harness() {
      const [v, setV] = useState<'a' | 'b'>('a');
      return (
        <Segmented<'a' | 'b'>
          label="Choice"
          value={v}
          onChange={setV}
          options={[
            { value: 'a', label: 'A', count: 1 },
            { value: 'b', label: 'B', count: 2 },
          ]}
        />
      );
    }
    render(<Harness />);
    const a = screen.getByRole('radio', { name: /A/ });
    a.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(screen.getByRole('radio', { name: /B/ })).toHaveAttribute('aria-checked', 'true');
  });
});

describe('ProviderMark and ModelMark', () => {
  it('draws a provider type as its brand and a generic type as a monogram', () => {
    const { container, rerender } = render(<ProviderMark type="openai" />);
    expect(container.querySelector('svg')).toHaveAttribute('data-mark', 'openai');
    expect(container.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
    rerender(<ProviderMark type="openai_compatible" />);
    expect(container.querySelector('svg')).toBeNull();
    expect(container.querySelector('[data-mark=monogram]')).toHaveTextContent('O');
    rerender(<ProviderMark type="anthropic_compatible" />);
    expect(container.querySelector('[data-mark=monogram]')).toHaveTextContent('A');
    rerender(<ProviderName type="anthropic" name="claude-east" />);
    expect(container).toHaveTextContent('claude-east');
    expect(container.querySelector('svg')).toHaveAttribute('data-mark', 'anthropic');
  });

  it("draws a model as its maker's mark, whatever serves it, and an unknown maker as a monogram", () => {
    const { container, rerender } = render(<ModelMark model="claude-sonnet" />);
    expect(container.querySelector('svg')).toHaveAttribute('data-mark', 'anthropic');
    rerender(<ModelMark model="mistral-large" />);
    expect(container.querySelector('svg')).toHaveAttribute('data-mark', 'mistral');
    rerender(<ModelMark model="chat" upstream="us.anthropic.claude-sonnet-4-5-v1:0" />);
    expect(container.querySelector('svg')).toHaveAttribute('data-mark', 'anthropic');
    rerender(<ModelMark model="house-model" ownedBy="meta" />);
    expect(container.querySelector('svg')).toHaveAttribute('data-mark', 'meta');
    rerender(<ModelMark model="house-model" />);
    expect(container.querySelector('svg')).toBeNull();
    expect(container.querySelector('[data-mark=monogram]')).toHaveTextContent('H');
  });

  it('keeps one pixel size per variant, for glyphs and monograms alike', () => {
    const { container, rerender } = render(<ModelMark model="gpt-4o" size="xs" />);
    expect(container.querySelector('svg')).toHaveAttribute('width', '12');
    expect(container.querySelector('svg')).toHaveClass('h-3', 'w-3');
    rerender(<ModelMark model="acme-7b" size="xs" />);
    expect(container.querySelector('[data-mark=monogram]')).toHaveClass('h-3', 'w-3');
    rerender(
      <MarkTile>
        <ModelMark model="acme-7b" size="lg" />
      </MarkTile>,
    );
    expect(container.querySelector('[data-mark=monogram]')).toHaveClass('h-[18px]');
  });
});

describe('Drawer with unsaved edits', () => {
  it('asks before discarding them, and closes at once without any', async () => {
    const onClose = vi.fn();
    const { rerender } = render(
      <Drawer open dirty onClose={onClose} title="Edit budget">
        <p>Form</p>
      </Drawer>,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    await userEvent.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    rerender(
      <Drawer open onClose={onClose} title="Edit budget">
        <p>Form</p>
      </Drawer>,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("asks on the form's Cancel too, in a drawer and in a dialog", async () => {
    const onClose = vi.fn();
    const { rerender } = render(
      <Drawer open dirty onClose={onClose} title="Edit key" footer={<CancelButton />}>
        <p>Form</p>
      </Drawer>,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('alertdialog', { name: 'Discard changes?' })).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    rerender(
      <Dialog open dirty onClose={onClose} title="Edit alias" footer={<CancelButton />}>
        <p>Form</p>
      </Dialog>,
    );
    await userEvent.keyboard('{Escape}');
    expect(screen.getByRole('alertdialog', { name: 'Discard changes?' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
