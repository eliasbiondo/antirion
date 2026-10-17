import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { Button, IconButton } from '../button';
import { Checkbox, RadioGroup, Switch } from '../choice';
import { Calendar, DatePicker, TimeField, addMonths, monthGrid, toDay } from '../date-picker';
import { placeFloating } from '../floating';
import { Field, NumberInput } from '../form';
import { Dialog } from '../overlay';
import { Combobox, MultiSelect, Select } from '../select';
import { Tooltip } from '../tooltip';

const FRUIT = [
  { value: 'apple', label: 'Apple' },
  { value: 'apricot', label: 'Apricot' },
  { value: 'banana', label: 'Banana' },
  { value: 'cherry', label: 'Cherry', disabled: true },
  { value: 'date', label: 'Date' },
];

function SelectHarness({
  onChange,
  searchable = false,
}: {
  onChange?: (v: string) => void;
  searchable?: boolean;
}) {
  const [value, setValue] = useState('banana');
  const Control = searchable ? Combobox : Select;
  return (
    <>
      <Field label="Fruit">
        <Control
          value={value}
          onChange={(v) => {
            setValue(v);
            onChange?.(v);
          }}
          options={FRUIT}
        />
      </Field>
      <button type="button">After</button>
    </>
  );
}

describe('Select', () => {
  it('is a labelled select-only combobox that opens on the selected option', async () => {
    render(<SelectHarness />);
    const trigger = screen.getByRole('combobox', { name: 'Fruit' });
    expect(trigger).toHaveAttribute('aria-haspopup', 'listbox');
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    expect(trigger).toHaveTextContent('Banana');
    trigger.focus();
    await userEvent.keyboard('{ArrowDown}');
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    const list = screen.getByRole('listbox', { name: 'Fruit' });
    expect(trigger).toHaveAttribute('aria-controls', list.id);
    const active = trigger.getAttribute('aria-activedescendant') ?? '';
    expect(document.getElementById(active)).toHaveTextContent('Banana');
    expect(within(list).getByRole('option', { name: 'Banana' })).toHaveAttribute('aria-selected', 'true');
    expect(trigger).toHaveFocus();
  });

  it('moves with the arrows past disabled options, Home and End, and chooses with Enter', async () => {
    const onChange = vi.fn();
    render(<SelectHarness onChange={onChange} />);
    const trigger = screen.getByRole('combobox', { name: 'Fruit' });
    trigger.focus();
    await userEvent.keyboard('{Enter}');
    const activeText = () =>
      document.getElementById(trigger.getAttribute('aria-activedescendant') ?? '')?.textContent;
    await userEvent.keyboard('{ArrowDown}');
    expect(activeText()).toBe('Date');
    await userEvent.keyboard('{ArrowDown}');
    expect(activeText()).toBe('Date');
    await userEvent.keyboard('{Home}');
    expect(activeText()).toBe('Apple');
    await userEvent.keyboard('{End}');
    expect(activeText()).toBe('Date');
    await userEvent.keyboard('{ArrowUp}');
    expect(activeText()).toBe('Banana');
    await userEvent.keyboard('{ArrowUp}{Enter}');
    expect(onChange).toHaveBeenLastCalledWith('apricot');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(trigger).toHaveTextContent('Apricot');
    expect(trigger).toHaveFocus();
  });

  it('jumps by typed letters, cycles on a repeated letter, and Escape keeps the value', async () => {
    const onChange = vi.fn();
    render(<SelectHarness onChange={onChange} />);
    const trigger = screen.getByRole('combobox', { name: 'Fruit' });
    trigger.focus();
    await userEvent.keyboard('a');
    const activeText = () =>
      document.getElementById(trigger.getAttribute('aria-activedescendant') ?? '')?.textContent;
    expect(screen.getByRole('listbox')).toBeInTheDocument();
    expect(activeText()).toBe('Apple');
    await userEvent.keyboard('a');
    expect(activeText()).toBe('Apricot');
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
    expect(trigger).toHaveTextContent('Banana');
  });

  it('chooses the active option on Tab and moves on', async () => {
    const onChange = vi.fn();
    render(<SelectHarness onChange={onChange} />);
    const trigger = screen.getByRole('combobox', { name: 'Fruit' });
    trigger.focus();
    await userEvent.keyboard('{ArrowDown}{ArrowDown}');
    await userEvent.tab();
    expect(onChange).toHaveBeenLastCalledWith('date');
    expect(screen.getByRole('button', { name: 'After' })).toHaveFocus();
  });

  it('picks with the mouse and closes on an outside press', async () => {
    const onChange = vi.fn();
    render(<SelectHarness onChange={onChange} />);
    await userEvent.click(screen.getByRole('combobox', { name: 'Fruit' }));
    await userEvent.click(screen.getByRole('option', { name: 'Cherry' }));
    expect(onChange).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('option', { name: 'Apple' }));
    expect(onChange).toHaveBeenCalledWith('apple');
    await userEvent.click(screen.getByRole('combobox', { name: 'Fruit' }));
    await userEvent.click(document.body);
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });

  it('in a dialog, Escape closes the list first and the dialog second', async () => {
    const onClose = vi.fn();
    render(
      <Dialog open onClose={onClose} title="Edit">
        <SelectHarness />
      </Dialog>,
    );
    const trigger = screen.getByRole('combobox', { name: 'Fruit' });
    trigger.focus();
    await userEvent.keyboard('{ArrowDown}');
    // The list mounts inside the modal dialog, so assistive technology can reach it.
    expect(screen.getByRole('dialog', { name: 'Edit' })).toContainElement(screen.getByRole('listbox'));
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await userEvent.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalled();
  });
});

describe('Select placeholder and empty option', () => {
  it('shows a placeholder as a prompt, never as an option or ticked', async () => {
    const onChange = vi.fn();
    render(
      <Field label="Provider">
        <Select value="" onChange={onChange} options={FRUIT} placeholder="Select a provider" />
      </Field>,
    );
    const trigger = screen.getByRole('combobox', { name: 'Provider' });
    expect(trigger).toHaveTextContent('Select a provider');
    await userEvent.click(trigger);
    const list = screen.getByRole('listbox', { name: 'Provider' });
    const options = within(list).getAllByRole('option');
    expect(options[0]).toHaveTextContent('Apple');
    expect(within(list).queryByRole('option', { name: 'Select a provider' })).toBeNull();
    expect(options.every((o) => o.getAttribute('aria-selected') !== 'true')).toBe(true);
    const active = trigger.getAttribute('aria-activedescendant') ?? '';
    expect(document.getElementById(active)).toHaveTextContent('Apple');
    await userEvent.keyboard('{Enter}');
    expect(onChange).toHaveBeenCalledWith('apple');
  });

  it('offers an empty option when nothing is a choice of its own, ticked while empty', async () => {
    const onChange = vi.fn();
    render(
      <Field label="Team">
        <Select value="" onChange={onChange} options={FRUIT} emptyOption="All teams" />
      </Field>,
    );
    const trigger = screen.getByRole('combobox', { name: 'Team' });
    expect(trigger).toHaveTextContent('All teams');
    await userEvent.click(trigger);
    const list = screen.getByRole('listbox', { name: 'Team' });
    expect(within(list).getAllByRole('option')[0]).toHaveTextContent('All teams');
    expect(within(list).getByRole('option', { name: 'All teams' })).toHaveAttribute('aria-selected', 'true');
  });
});

describe('Combobox', () => {
  it('filters as you type in its search field and picks with Enter', async () => {
    const onChange = vi.fn();
    render(<SelectHarness searchable onChange={onChange} />);
    const trigger = screen.getByRole('combobox', { name: 'Fruit' });
    await userEvent.click(trigger);
    const search = screen.getByRole('combobox', { name: 'Search Fruit' });
    expect(search).toHaveFocus();
    expect(search).toHaveAttribute('aria-autocomplete', 'list');
    await userEvent.type(search, 'ap');
    const list = screen.getByRole('listbox');
    expect(
      within(list)
        .getAllByRole('option')
        .map((o) => o.textContent),
    ).toEqual(['Apple', 'Apricot']);
    await userEvent.keyboard('{ArrowDown}');
    expect(document.getElementById(search.getAttribute('aria-activedescendant') ?? '')).toHaveTextContent(
      'Apricot',
    );
    await userEvent.keyboard('{Enter}');
    expect(onChange).toHaveBeenCalledWith('apricot');
    expect(trigger).toHaveFocus();
    await userEvent.click(trigger);
    await userEvent.type(screen.getByRole('combobox', { name: 'Search Fruit' }), 'zzz');
    expect(screen.getByText('No matches')).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });
});

function MultiHarness({ creatable = false }: { creatable?: boolean }) {
  const [value, setValue] = useState<string[]>(['apple']);
  return (
    <Field label="Fruit">
      <MultiSelect noun="fruit" value={value} onChange={setValue} options={FRUIT} creatable={creatable} />
    </Field>
  );
}

describe('MultiSelect', () => {
  it('shows picks as chips, toggles options and stays open, removes with the chip or Backspace', async () => {
    render(<MultiHarness />);
    expect(screen.getByRole('button', { name: 'Remove fruit Apple' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('combobox', { name: 'Fruit' }));
    const list = screen.getByRole('listbox');
    expect(list).toHaveAttribute('aria-multiselectable', 'true');
    await userEvent.click(within(list).getByRole('option', { name: 'Banana' }));
    expect(within(list).getByRole('option', { name: 'Banana' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('button', { name: 'Remove fruit Banana' })).toBeInTheDocument();
    await userEvent.keyboard('{ArrowUp}{Enter}');
    expect(within(list).getByRole('option', { name: 'Apricot' })).toHaveAttribute('aria-selected', 'true');
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Remove fruit Apple' }));
    expect(screen.queryByRole('button', { name: 'Remove fruit Apple' })).not.toBeInTheDocument();
    screen.getByRole('combobox', { name: 'Fruit' }).focus();
    await userEvent.keyboard('{Backspace}');
    expect(screen.queryByRole('button', { name: 'Remove fruit Apricot' })).not.toBeInTheDocument();
  });

  it('adds a typed value when creatable', async () => {
    render(<MultiHarness creatable />);
    await userEvent.click(screen.getByRole('combobox', { name: 'Fruit' }));
    await userEvent.type(screen.getByRole('combobox', { name: 'Search or add Fruit' }), 'kiwi{Enter}');
    expect(screen.getByRole('button', { name: 'Remove fruit kiwi' })).toBeInTheDocument();
  });
});

describe('Checkbox, radio group and switch', () => {
  it('a checkbox toggles with Space and its label, not Enter, and says mixed', async () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <Checkbox checked={false} onChange={onChange} label="Email me" description="Once a day" />,
    );
    const box = screen.getByRole('checkbox', { name: 'Email me' });
    expect(box).toHaveAccessibleDescription('Once a day');
    expect(box).not.toBeChecked();
    box.focus();
    await userEvent.keyboard('{Enter}');
    expect(onChange).not.toHaveBeenCalled();
    await userEvent.keyboard(' ');
    expect(onChange).toHaveBeenLastCalledWith(true);
    await userEvent.click(screen.getByText('Email me'));
    expect(onChange).toHaveBeenCalledTimes(2);
    rerender(<Checkbox checked="mixed" onChange={onChange} label="Email me" />);
    expect(screen.getByRole('checkbox', { name: 'Email me' })).toHaveAttribute('aria-checked', 'mixed');
  });

  it('radio cards are named by their label and described by their line', () => {
    render(
      <RadioGroup
        label="Strategy"
        variant="cards"
        value="a"
        onChange={() => undefined}
        options={[{ value: 'a', label: 'Weighted split', description: 'Split traffic by weight.' }]}
      />,
    );
    const card = screen.getByRole('radio', { name: 'Weighted split' });
    expect(card).toHaveAccessibleDescription('Split traffic by weight.');
  });

  it('a radio group is one tab stop; the arrows move the choice and wrap', async () => {
    function Harness() {
      const [v, setV] = useState<'a' | 'b' | 'c'>('b');
      return (
        <>
          <RadioGroup
            label="Strategy"
            value={v}
            onChange={setV}
            options={[
              { value: 'a', label: 'First' },
              { value: 'b', label: 'Second', description: 'The default' },
              { value: 'c', label: 'Third' },
            ]}
          />
          <button type="button">After</button>
        </>
      );
    }
    render(<Harness />);
    const group = screen.getByRole('radiogroup', { name: 'Strategy' });
    const second = within(group).getByRole('radio', { name: 'Second' });
    expect(second).toBeChecked();
    expect(second).toHaveAccessibleDescription('The default');
    await userEvent.tab();
    expect(second).toHaveFocus();
    await userEvent.keyboard('{ArrowDown}');
    expect(within(group).getByRole('radio', { name: 'Third' })).toBeChecked();
    expect(within(group).getByRole('radio', { name: 'Third' })).toHaveFocus();
    await userEvent.keyboard('{ArrowRight}');
    expect(within(group).getByRole('radio', { name: 'First' })).toBeChecked();
    await userEvent.keyboard('{ArrowUp}');
    expect(within(group).getByRole('radio', { name: 'Third' })).toBeChecked();
    await userEvent.tab();
    expect(screen.getByRole('button', { name: 'After' })).toHaveFocus();
  });

  it('a switch flips with Space, and a disabled one says why', async () => {
    const onChange = vi.fn();
    const { rerender } = render(<Switch checked={false} onChange={onChange} label="Turn openai on" />);
    screen.getByRole('switch', { name: 'Turn openai on' }).focus();
    await userEvent.keyboard(' ');
    expect(onChange).toHaveBeenCalledWith(true);
    rerender(
      <Switch
        checked
        onChange={onChange}
        label="Turn openai off"
        disabled
        disabledReason="You don't have permission to change providers."
      />,
    );
    expect(screen.getByRole('switch', { name: 'Turn openai off' })).toHaveAccessibleDescription(
      "You don't have permission to change providers.",
    );
  });
});

describe('Tooltip', () => {
  it('shows on hover after a delay and at once on keyboard focus, describes its trigger, and Escape hides it', async () => {
    render(
      <Tooltip content="Download as CSV">
        <button type="button">Export</button>
      </Tooltip>,
    );
    const button = screen.getByRole('button', { name: 'Export' });
    await userEvent.hover(button);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    expect(await screen.findByRole('tooltip')).toHaveTextContent('Download as CSV');
    expect(button).toHaveAccessibleDescription('Download as CSV');
    await userEvent.unhover(button);
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    await userEvent.tab();
    expect(button).toHaveFocus();
    expect(screen.getByRole('tooltip')).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    expect(button).toHaveFocus();
  });

  it('an icon button shows its label, and a disabled button its reason', async () => {
    render(
      <>
        <IconButton icon="x" label="Close" />
        <Button disabled disabledReason="Nothing changed yet">
          Save
        </Button>
      </>,
    );
    const close = screen.getByRole('button', { name: 'Close' });
    expect(close).not.toHaveAttribute('title');
    await userEvent.hover(close);
    expect(await screen.findByRole('tooltip')).toHaveTextContent('Close');
    const save = screen.getByRole('button', { name: 'Save' });
    expect(save).not.toHaveAttribute('title');
    expect(save).toHaveAccessibleDescription('Nothing changed yet');
    await userEvent.unhover(close);
    await userEvent.hover(save.parentElement ?? save);
    expect(await screen.findByRole('tooltip')).toHaveTextContent('Nothing changed yet');
  });
});

describe('dates', () => {
  it('lays out months from Monday and adds months without overflowing', () => {
    const grid = monthGrid(new Date(2026, 9, 14));
    expect(grid).toHaveLength(42);
    expect(toDay(grid[0] ?? new Date())).toBe('2026-09-28');
    expect(toDay(addMonths(new Date(2026, 0, 31), 1))).toBe('2026-02-28');
  });

  it('the calendar grid moves by day, week and month from the keyboard and picks with Enter', async () => {
    const onPick = vi.fn();
    function Harness() {
      const [focus, setFocus] = useState('2026-10-14');
      return (
        <Calendar focus={focus} onFocusChange={setFocus} onPick={onPick} selected="2026-10-14" autoFocus />
      );
    }
    render(<Harness />);
    const grid = screen.getByRole('grid', { name: 'October 2026' });
    expect(within(grid).getByRole('button', { name: 'Wednesday, 14 October 2026' })).toHaveFocus();
    await userEvent.keyboard('{ArrowRight}');
    expect(within(grid).getByRole('button', { name: 'Thursday, 15 October 2026' })).toHaveFocus();
    await userEvent.keyboard('{ArrowDown}');
    expect(within(grid).getByRole('button', { name: 'Thursday, 22 October 2026' })).toHaveFocus();
    await userEvent.keyboard('{Home}');
    expect(within(grid).getByRole('button', { name: 'Monday, 19 October 2026' })).toHaveFocus();
    await userEvent.keyboard('{PageDown}');
    expect(screen.getByRole('grid', { name: 'November 2026' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Thursday, 19 November 2026' })).toHaveFocus();
    await userEvent.keyboard('{Enter}');
    expect(onPick).toHaveBeenCalledWith('2026-11-19');
  });

  it('a date picker opens a calendar dialog on the chosen day and gives focus back after a pick', async () => {
    function Harness() {
      const [v, setV] = useState('2026-10-14');
      return (
        <Field label="Expires">
          <DatePicker value={v} onChange={setV} />
        </Field>
      );
    }
    render(<Harness />);
    const field = screen.getByRole('button', { name: 'Expires' });
    expect(field).toHaveTextContent('14 Oct 2026');
    await userEvent.click(field);
    const dialog = screen.getByRole('dialog', { name: 'Choose a date' });
    expect(within(dialog).getByRole('button', { name: 'Wednesday, 14 October 2026' })).toHaveFocus();
    await userEvent.keyboard('{ArrowLeft}{Enter}');
    expect(screen.queryByRole('dialog', { name: 'Choose a date' })).not.toBeInTheDocument();
    expect(field).toHaveTextContent('13 Oct 2026');
    expect(field).toHaveFocus();
    await userEvent.click(field);
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog', { name: 'Choose a date' })).not.toBeInTheDocument();
    expect(field).toHaveTextContent('13 Oct 2026');
  });

  it('a time field steps with the arrows', async () => {
    const onChange = vi.fn();
    render(<TimeField aria-label="From time" value="09:50" onChange={onChange} />);
    screen.getByRole('textbox', { name: 'From time' }).focus();
    await userEvent.keyboard('{ArrowUp}');
    expect(onChange).toHaveBeenLastCalledWith('10:05');
    await userEvent.keyboard('{Shift>}{ArrowDown}{/Shift}');
    expect(onChange).toHaveBeenLastCalledWith('08:50');
  });
});

describe('NumberInput', () => {
  it('is a spin button without native spinners: arrows, Page keys, limits and steppers', async () => {
    function Harness() {
      const [v, setV] = useState('40');
      return (
        <Field label="Rate limit">
          <NumberInput value={v} onChange={setV} min={0} max={100} step={10} steppers />
        </Field>
      );
    }
    render(<Harness />);
    const input = screen.getByRole('spinbutton', { name: 'Rate limit' });
    expect(input).toHaveAttribute('type', 'text');
    expect(input).toHaveAttribute('aria-valuenow', '40');
    input.focus();
    await userEvent.keyboard('{ArrowUp}');
    expect(input).toHaveValue('50');
    await userEvent.keyboard('{PageUp}');
    expect(input).toHaveValue('100');
    await userEvent.keyboard('{Home}');
    expect(input).toHaveValue('0');
    await userEvent.click(screen.getByRole('button', { name: 'Increase value' }));
    expect(input).toHaveValue('10');
  });
});

describe('floating placement', () => {
  const viewport = { width: 1000, height: 800 };
  it('opens below, flips above when there is more room there, and stays inside the viewport', () => {
    const below = placeFloating(
      { top: 100, bottom: 132, left: 50, right: 250, width: 200 },
      { width: 200, height: 300 },
      viewport,
    );
    expect(below.side).toBe('bottom');
    expect(below.top).toBe(138);
    const flipped = placeFloating(
      { top: 700, bottom: 732, left: 900, right: 990, width: 90 },
      { width: 240, height: 300 },
      viewport,
    );
    expect(flipped.side).toBe('top');
    expect(flipped.top).toBe(394);
    expect(flipped.left).toBe(752);
  });
});
