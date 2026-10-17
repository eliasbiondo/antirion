import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { SelectableList } from '@/components/ui/list-detail';

describe('SelectableList focus', () => {
  it('moves focus to a row selected from outside', () => {
    const items = ['ada-laptop', 'qa-key'];
    const list = (selected: string | null) => (
      <SelectableList
        label="Keys"
        items={items}
        keyOf={(k) => k}
        selectedKey={selected}
        onSelect={() => undefined}
        renderItem={(k) => k}
      />
    );
    const { rerender } = render(list(null));
    screen.getByRole('option', { name: 'ada-laptop' }).focus();
    rerender(list('qa-key'));
    expect(document.activeElement).toBe(screen.getByRole('option', { name: 'qa-key' }));
  });
});
