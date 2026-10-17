import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { fillerClass, fillerCounts, Section, TileGrid } from '../layout';
import { EmptyAction, EmptyPill, EmptyState, NoMatch } from '../states';

describe('EmptyState', () => {
  it('is one pill with a muted line and quiet actions: no icon, no headline', async () => {
    const create = vi.fn();
    const { container } = render(
      <EmptyState
        title="No API keys yet"
        description="Apps send requests with a key."
        action={<EmptyAction onClick={create}>Create key</EmptyAction>}
        secondaryAction={<EmptyAction href="/docs/">Read the guide</EmptyAction>}
      />,
    );
    const pill = screen.getByText('No API keys yet');
    expect(pill.tagName).toBe('SPAN');
    expect(pill).toHaveClass('rounded-control', 'border', 'border-line', 'text-caption', 'text-muted');
    expect(screen.getByText('Apps send requests with a key.')).toHaveClass('text-muted');
    expect(container.querySelector('svg')).toBeNull();
    expect(container.querySelector('h1, h2, h3, h4, .font-semibold')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Create key' }));
    expect(create).toHaveBeenCalledOnce();
    expect(screen.getByRole('link', { name: 'Read the guide' })).toHaveAttribute(
      'href',
      expect.stringContaining('/docs'),
    );
  });

  it('differs between sizes only in vertical room', () => {
    const { container, rerender } = render(<EmptyState title="Nothing yet" size="sm" />);
    const small = (container.firstChild as HTMLElement).className;
    rerender(<EmptyState title="Nothing yet" />);
    const page = (container.firstChild as HTMLElement).className;
    expect(small.replace('py-6', '')).toBe(page.replace('py-16', ''));
  });

  it('offers to clear filters when they match nothing', async () => {
    const clear = vi.fn();
    render(<NoMatch onClear={clear} />);
    expect(screen.getByText('No results match these filters')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Clear filters' }));
    expect(clear).toHaveBeenCalledOnce();
  });

  it('keeps a disabled action with its reason', () => {
    render(
      <EmptyAction disabled disabledReason="It needs the alerts:write permission.">
        New rule
      </EmptyAction>,
    );
    expect(screen.getByRole('button', { name: 'New rule' })).toBeDisabled();
  });

  it('draws the same pill on its own, for charts and popovers', () => {
    render(<EmptyPill>No matches</EmptyPill>);
    expect(screen.getByText('No matches')).toHaveClass('rounded-control', 'border-line', 'text-muted');
  });
});

describe('TileGrid', () => {
  it('counts the empty cells of a short last row at each breakpoint', () => {
    expect(fillerCounts(4, { base: 1, lg: 2, xl: 3 })).toEqual([
      ['base', 0],
      ['lg', 0],
      ['xl', 2],
    ]);
    expect(fillerCounts(5, { base: 2, md: 3, xl: 4 })).toEqual([
      ['base', 1],
      ['md', 1],
      ['xl', 3],
    ]);
    expect(fillerCounts(6, { base: 1, sm: 2, lg: 3 })).toEqual([
      ['base', 0],
      ['sm', 0],
      ['lg', 0],
    ]);
  });

  it('shows each filler only where it completes the row', () => {
    const counts = fillerCounts(4, { base: 1, lg: 2, xl: 3 });
    expect(fillerClass(0, counts)).toBe('hidden xl:block');
    expect(fillerClass(1, counts)).toBe('hidden xl:block');
    const five = fillerCounts(5, { base: 2, md: 3, xl: 4 });
    expect(fillerClass(0, five)).toBe('block');
    expect(fillerClass(2, five)).toBe('hidden xl:block');
  });

  it('fills a partial last row with blank tiles and sets the columns', () => {
    const { container } = render(
      <TileGrid as="ul" label="Teams" cols={{ base: 1, lg: 2, xl: 3 }}>
        {['a', 'b', 'c', 'd'].map((n) => (
          <li key={n}>{n}</li>
        ))}
      </TileGrid>,
    );
    const grid = screen.getByRole('list', { name: 'Teams' });
    expect(grid).toHaveClass(
      'grid',
      'gap-px',
      'bg-line',
      'border-b',
      'grid-cols-1',
      'lg:grid-cols-2',
      'xl:grid-cols-3',
    );
    const fillers = container.querySelectorAll('li[aria-hidden="true"]');
    expect(fillers).toHaveLength(2);
    expect(fillers[0]).toHaveClass('bg-raised', 'hidden', 'xl:block');
  });

  it('adds no filler when every row is full', () => {
    const { container } = render(
      <TileGrid cols={{ base: 1, md: 2 }}>
        <div>a</div>
        <div>b</div>
      </TileGrid>,
    );
    expect(container.querySelectorAll('[aria-hidden="true"]')).toHaveLength(0);
  });
});

describe('Section', () => {
  it('is flat and full-bleed: a header row, a hairline under it, no card', () => {
    render(
      <Section
        title="Roles"
        subtitle="Assign them on a user's page."
        actions={<button type="button">Create role</button>}
        flush
      >
        <p>rows</p>
      </Section>,
    );
    const section = screen.getByRole('heading', { name: 'Roles' }).closest('section');
    expect(section).toHaveClass('border-b', 'border-line');
    expect(section?.className).not.toMatch(/rounded|border-x|\bborder\b(?!-)/);
    expect(screen.getByText("Assign them on a user's page.")).toHaveClass('text-muted');
    expect(screen.getByRole('button', { name: 'Create role' })).toBeInTheDocument();
  });
});
