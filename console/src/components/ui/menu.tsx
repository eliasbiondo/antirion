'use client';

import { useRef, useState } from 'react';

import { cn } from '@/lib/cn';

import { Button, IconButton, type ButtonSize, type ButtonVariant } from './button';
import { Icon, type IconName } from './icon';
import { menuKeyHandler, Popover } from './overlay';

export interface MenuItem {
  label: string;
  icon?: IconName;
  onSelect: () => void;
  /** Destructive: drawn in red, and listed last after a separator. */
  danger?: boolean;
  disabled?: boolean;
  /** Why it is disabled, as the item's tooltip. */
  disabledReason?: string | undefined;
}

export interface MenuButtonProps {
  items: MenuItem[];
  /** The trigger's text ("More", "Actions"); with `iconOnly` it is the accessible name. */
  label?: string;
  icon?: IconName;
  iconOnly?: boolean;
  variant?: ButtonVariant;
  size?: ButtonSize;
  align?: 'left' | 'right';
}

/**
 * A "More" menu for secondary and rare actions, so a header or a detail pane keeps one primary
 * and at most two secondary buttons. Destructive items go last, in red, after a separator.
 */
export function MenuButton({
  items,
  label = 'More',
  icon = 'more',
  iconOnly = false,
  variant = 'secondary',
  size = 'md',
  align = 'right',
}: MenuButtonProps) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const safe = items.filter((i) => !i.danger);
  const danger = items.filter((i) => i.danger);
  const row = (item: MenuItem) => (
    <span key={item.label} className="block [&>button:disabled]:pointer-events-none">
      <button
        type="button"
        role="menuitem"
        disabled={item.disabled}
        aria-disabled={item.disabled || undefined}
        onClick={() => {
          setOpen(false);
          item.onSelect();
        }}
        className={cn(
          'flex w-full items-center gap-2.5 rounded-sm px-2.5 py-1.5 text-left text-ui hover:bg-hover focus:bg-hover',
          'disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent',
          item.danger ? 'text-error' : 'text-fg',
        )}
      >
        {item.icon ? (
          <Icon name={item.icon} size={16} className={item.danger ? 'text-error' : 'text-muted'} />
        ) : null}
        <span className="flex min-w-0 flex-col">
          <span>{item.label}</span>
          {item.disabled && item.disabledReason ? (
            <span className="text-caption text-muted">{item.disabledReason}</span>
          ) : null}
        </span>
      </button>
    </span>
  );
  return (
    <div className="relative">
      {iconOnly ? (
        <IconButton
          ref={anchor}
          icon={icon}
          label={label}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        />
      ) : (
        <Button
          ref={anchor}
          variant={variant}
          size={size}
          iconAfter="chevronDown"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {label}
        </Button>
      )}
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        align={align}
        label={label}
        role="menu"
        className="min-w-[200px] p-1"
      >
        <div onKeyDown={menuKeyHandler}>
          {safe.map(row)}
          {danger.length > 0 && safe.length > 0 ? <div className="my-1 border-t border-line" /> : null}
          {danger.map(row)}
        </div>
      </Popover>
    </div>
  );
}
