import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { chatUrl, curlSnippet } from '@/features/overview/setup';
import { ApiError } from '@/lib/api/client';
import { activeNav, GO_KEYS, usesTimeRange } from '@/lib/nav';

import { niceScale } from '../charts';
import { ConfirmDialog } from '../confirm';
import { KpiTile } from '../kpi';
import { PageHeader } from '../layout';
import { MenuButton } from '../menu';
import { SavedViews } from '../saved-views';
import { ErrorState, isNotFound, isUnavailable } from '../states';
import { CountNote } from '../table';

function apiError(status: number, message = 'boom', operation = 'GET /analytics/usage'): ApiError {
  return new ApiError({ status, code: 'x', message, requestId: '01a1-req', operation });
}

describe('ErrorState', () => {
  it('says a contract-only read is not available, with the endpoint only behind Details', () => {
    render(<ErrorState error={apiError(404)} subject="Analytics" />);
    expect(screen.getByText("Analytics isn't available on this gateway yet")).toBeInTheDocument();
    expect(screen.getByText('Upgrade the gateway to use it.')).toBeInTheDocument();
    const details = screen.getByText('Details').closest('details');
    expect(details).not.toHaveAttribute('open');
    expect(within(details as HTMLElement).getByText('GET /analytics/usage')).toBeInTheDocument();
  });

  it('says an entity that is not there was not found, never that the gateway needs an upgrade', () => {
    const missing = apiError(404, 'The resource does not exist.', 'GET /logs/{id}');
    expect(isUnavailable(missing)).toBe(false);
    expect(isNotFound(missing)).toBe(true);
    expect(isUnavailable(apiError(404))).toBe(true);
    render(<ErrorState error={missing} subject="This request" />);
    expect(screen.getByText("We couldn't find this request")).toBeInTheDocument();
    expect(screen.queryByText(/Upgrade the gateway/)).not.toBeInTheDocument();
  });

  it('words a 403 as access and a failure as "couldn\'t load" with a retry', async () => {
    const { rerender } = render(<ErrorState error={apiError(403)} subject="Alerts" />);
    expect(screen.getByText("You don't have access to alerts")).toBeInTheDocument();
    const retry = vi.fn();
    rerender(<ErrorState error={apiError(500, 'store down')} subject="Alerts" onRetry={retry} />);
    expect(screen.getByText("Couldn't load alerts")).toBeInTheDocument();
    expect(screen.getByText('store down').closest('details')).not.toHaveAttribute('open');
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(retry).toHaveBeenCalled();
  });
});

describe('KpiTile', () => {
  it('never shows a zero it did not read', () => {
    render(<KpiTile label="Spend" value="$0.00" state="unavailable" />);
    expect(screen.queryByText('$0.00')).not.toBeInTheDocument();
    expect(screen.getByText('Not available')).toBeInTheDocument();
  });

  it('shows the figure, the context and the change with its comparison', async () => {
    render(
      <KpiTile
        label="Spend"
        value="$53.01"
        context="$0.0059/req"
        delta={{ text: '+10.0%', tone: 'bad', title: 'vs the previous 6 hours' }}
      />,
    );
    expect(screen.getByText('$53.01')).toBeInTheDocument();
    expect(screen.getByText('$0.0059/req')).toBeInTheDocument();
    const delta = screen.getByText(/\+10\.0%/);
    expect(delta).toHaveTextContent('+10.0% vs the previous 6 hours');
    await userEvent.hover(delta);
    expect(await screen.findByRole('tooltip')).toHaveTextContent('vs the previous 6 hours');
  });

  it('keeps its place with a hint while there is no data yet', () => {
    render(<KpiTile label="Requests" state="empty" emptyHint="After the first request" />);
    expect(screen.getByText('After the first request')).toBeInTheDocument();
  });
});

describe('PageHeader', () => {
  it('reads "title · purpose · context" with the actions apart', () => {
    render(
      <PageHeader
        title="Overview"
        description="Live traffic, spend and health"
        context="Last 6 hours"
        actions={<button type="button">Export</button>}
      />,
    );
    expect(screen.getByRole('heading', { level: 1, name: 'Overview' })).toBeInTheDocument();
    expect(screen.getByText('Live traffic, spend and health')).toBeInTheDocument();
    expect(screen.getByText('Last 6 hours')).toBeInTheDocument();
  });
});

describe('ConfirmDialog', () => {
  it('starts on Cancel, names the object and fills the danger button', () => {
    render(
      <ConfirmDialog
        open
        onClose={() => undefined}
        title="Revoke key support-bot?"
        objectName="support-bot"
        effect="Apps using this key will stop working."
        confirmLabel="Revoke key"
        onConfirm={() => Promise.resolve()}
      />,
    );
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus();
    expect(screen.getByText('support-bot')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Revoke key' }).className).toContain('bg-error');
    expect(screen.queryByLabelText(/Reason/)).not.toBeInTheDocument();
  });
});

describe('CountNote', () => {
  it('counts in the right number and hides on an empty list', () => {
    const { rerender, container } = render(<CountNote shown={1} total={1} noun="keys" />);
    expect(screen.getByText('1 key')).toBeInTheDocument();
    rerender(<CountNote shown={2} total={5} noun="policies" />);
    expect(screen.getByText('2 of 5 policies')).toBeInTheDocument();
    rerender(<CountNote shown={0} total={0} noun="keys" />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe('MenuButton', () => {
  it('lists destructive items last', async () => {
    const onDelete = vi.fn();
    render(
      <MenuButton
        items={[
          { label: 'Delete key', danger: true, onSelect: onDelete },
          { label: 'Copy link', onSelect: () => undefined },
        ]}
      />,
    );
    await userEvent.click(screen.getByRole('button', { name: 'More' }));
    const items = screen.getAllByRole('menuitem').map((i) => i.textContent);
    expect(items).toEqual(['Copy link', 'Delete key']);
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete key' }));
    expect(onDelete).toHaveBeenCalled();
  });
});

describe('SavedViews', () => {
  it('lists starred views first and marks the active one', async () => {
    const onSelect = vi.fn();
    render(
      <SavedViews
        views={[
          { id: 'misses', name: 'Cache misses', starred: false },
          { id: 'all', name: 'All requests', starred: true, active: true },
        ]}
        onSelect={onSelect}
      />,
    );
    const buttons = screen.getAllByRole('button');
    expect(buttons[0]).toHaveTextContent('All requests');
    expect(buttons[0]).toHaveAttribute('aria-current', 'true');
    await userEvent.click(screen.getByRole('button', { name: /Cache misses/ }));
    expect(onSelect).toHaveBeenCalledWith('misses');
  });
});

describe('charts', () => {
  it('rounds the axis to readable steps', () => {
    expect(niceScale(183, 4)).toEqual({ top: 200, ticks: [0, 50, 100, 150, 200] });
    expect(niceScale(0, 4).top).toBe(1);
  });
});

describe('navigation', () => {
  it('keeps the prototype groups and reaches Providers', () => {
    expect(activeNav('/providers/')).toBe('providers');
    expect(activeNav('/providers')).toBe('providers');
    expect(activeNav('/')).toBe('overview');
    expect(usesTimeRange('/')).toBe(true);
    expect(usesTimeRange('/keys/')).toBe(false);
    expect(GO_KEYS.k).toBe('/keys/');
  });
});

describe('first request snippet', () => {
  it('builds the chat completions call from the gateway address', () => {
    expect(chatUrl('https://gw.example.com/')).toBe('https://gw.example.com/v1/chat/completions');
    expect(chatUrl('https://gw.example.com/v1')).toBe('https://gw.example.com/v1/chat/completions');
    const snippet = curlSnippet('http://127.0.0.1:20580', 'gpt-4o');
    expect(snippet).toContain('curl http://127.0.0.1:20580/v1/chat/completions');
    expect(snippet).toContain('"model": "gpt-4o"');
    expect(snippet).toContain('$ANTIRION_API_KEY');
  });
});
