'use client';

import {
  createContext,
  use,
  useEffect,
  useEffectEvent,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';

import { cn } from '@/lib/cn';

import { Button, IconButton } from './button';
import { useFloating, usePortalTarget } from './floating';
import { useLayer } from './layers';

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Traps focus inside `ref` while `open`, closes on Escape and gives focus back to the element that
 * had it before opening. With another overlay open on top (a listbox, a confirmation), that one
 * answers the keys first. Returns whether this overlay is the top one.
 */
export function useFocusTrap(
  ref: RefObject<HTMLElement | null>,
  open: boolean,
  onClose: () => void,
): () => boolean {
  const close = useEffectEvent(onClose);
  const isTop = useLayer(open);
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const node = ref.current;
    const first =
      node?.querySelector<HTMLElement>('[data-autofocus]') ?? node?.querySelector<HTMLElement>(FOCUSABLE);
    (first ?? node)?.focus();
    function onKey(e: KeyboardEvent) {
      if (!isTop()) return;
      if (e.key === 'Escape') {
        e.stopPropagation();
        close();
        return;
      }
      if (e.key !== 'Tab' || !node) return;
      const items = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
        (el) => el.offsetParent !== null,
      );
      if (items.length === 0) return;
      const firstItem = items[0];
      const lastItem = items[items.length - 1];
      if (e.shiftKey && document.activeElement === firstItem) {
        e.preventDefault();
        lastItem?.focus();
      } else if (!e.shiftKey && document.activeElement === lastItem) {
        e.preventDefault();
        firstItem?.focus();
      }
    }
    document.addEventListener('keydown', onKey, true);
    return () => {
      document.removeEventListener('keydown', onKey, true);
      if (previous?.isConnected) previous.focus();
    };
  }, [open, ref, isTop]);
  return isTop;
}

/**
 * Overlays open only after user interaction or a client-side state, never in static HTML. They
 * mount in the body, or in `target` (the modal dialog that holds their trigger).
 */
export function Portal({ children, target }: { children: ReactNode; target?: HTMLElement | null }) {
  if (typeof document === 'undefined') return null;
  return createPortal(children, target ?? document.body);
}

/**
 * Closes on a click that lands outside the overlay unless a newer overlay is on top (the click
 * then belongs to that one), and only when the press also started outside.
 */
export function useBackdropClose(isTop: () => boolean, onClose: () => void) {
  const pressedOnTop = useRef(false);
  return {
    onMouseDown: () => {
      pressedOnTop.current = isTop();
    },
    onClick: () => {
      if (pressedOnTop.current) onClose();
      pressedOnTop.current = false;
    },
  };
}

/** The close of the drawer or dialog around a form: it asks first when the form has unsaved edits. */
const GuardedClose = createContext<(() => void) | null>(null);

/**
 * A form's Cancel: it closes the drawer or dialog it is in the way Escape and ✕ do, so unsaved
 * edits are confirmed first. Outside one, it calls `onClick`.
 */
export function CancelButton({
  onClick,
  children = 'Cancel',
}: {
  onClick?: () => void;
  children?: ReactNode;
}) {
  const close = use(GuardedClose);
  return (
    <Button variant="ghost" onClick={close ?? onClick}>
      {children}
    </Button>
  );
}

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  size?: 'sm' | 'md' | 'lg' | 'xl';
  /** `alertdialog` for confirmations of destructive actions. */
  role?: 'dialog' | 'alertdialog';
  /** Edits not saved yet: Escape, ✕, a click outside and Cancel ask "Discard changes?" first. */
  dirty?: boolean;
}

const DIALOG_SIZE = { sm: 'max-w-[420px]', md: 'max-w-[560px]', lg: 'max-w-[760px]', xl: 'max-w-[1040px]' };

export function Dialog({
  open,
  onClose,
  title,
  description,
  children,
  footer,
  size = 'md',
  role = 'dialog',
  dirty = false,
}: DialogProps) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descId = useId();
  const [asking, setAsking] = useState(false);
  const close = () => (dirty ? setAsking(true) : onClose());
  const isTop = useFocusTrap(ref, open && !asking, close);
  const backdrop = useBackdropClose(isTop, close);
  if (!open) return null;
  return (
    <Portal>
      <GuardedClose value={close}>
        <div className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto bg-overlay p-4 pt-[8vh]">
          <div className="fixed inset-0" aria-hidden="true" {...backdrop} />
          <div
            ref={ref}
            role={role}
            aria-modal="true"
            aria-labelledby={titleId}
            aria-describedby={description ? descId : undefined}
            tabIndex={-1}
            className={cn(
              'relative flex w-full animate-fade-in flex-col rounded-container border border-line-strong bg-raised shadow-pop',
              DIALOG_SIZE[size],
            )}
          >
            <div className="flex items-start justify-between gap-4 border-b border-line px-5 py-4">
              <div>
                <h2 id={titleId} className="text-title font-semibold text-fg">
                  {title}
                </h2>
                {description ? (
                  <p id={descId} className="mt-1 text-ui text-muted">
                    {description}
                  </p>
                ) : null}
              </div>
              <IconButton icon="x" label="Close" onClick={close} />
            </div>
            <div className="max-h-[70vh] overflow-y-auto px-5 py-4">{children}</div>
            {footer ? (
              <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3">
                {footer}
              </div>
            ) : null}
          </div>
        </div>
        {asking ? (
          <DiscardChanges
            onKeep={() => setAsking(false)}
            onDiscard={() => {
              setAsking(false);
              onClose();
            }}
          />
        ) : null}
      </GuardedClose>
    </Portal>
  );
}

export interface DrawerProps {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  subtitle?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: 'md' | 'lg';
  /** Rendered next to the close button, such as "Copy link". */
  headerActions?: ReactNode;
  /**
   * Edits not saved yet: closing by Escape, the close button, a click outside or the form's
   * `CancelButton` asks "Discard changes?" first. A successful save closes through `onClose` directly.
   */
  dirty?: boolean;
}

/** "Discard changes?": the question an edit drawer asks before it closes with unsaved edits. */
function DiscardChanges({ onKeep, onDiscard }: { onKeep: () => void; onDiscard: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useFocusTrap(ref, true, onKeep);
  return (
    <div className="fixed inset-0 z-[55] flex items-center justify-center bg-overlay p-4">
      <div
        ref={ref}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className="w-full max-w-[400px] animate-fade-in rounded-container border border-line-strong bg-raised p-5 shadow-pop"
      >
        <h2 id={titleId} className="text-title font-semibold text-fg">
          Discard changes?
        </h2>
        <p className="mt-2 text-ui text-muted">What you changed here has not been saved.</p>
        <div className="mt-4 flex justify-end gap-2">
          <Button variant="ghost" onClick={onKeep} data-autofocus>
            Keep editing
          </Button>
          <Button variant="danger" onClick={onDiscard}>
            Discard
          </Button>
        </div>
      </div>
    </div>
  );
}

/** A right-side panel over the page for forms and details. */
export function Drawer({
  open,
  onClose,
  title,
  subtitle,
  children,
  footer,
  width = 'md',
  headerActions,
  dirty = false,
}: DrawerProps) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const [asking, setAsking] = useState(false);
  const close = () => (dirty ? setAsking(true) : onClose());
  const isTop = useFocusTrap(ref, open && !asking, close);
  const backdrop = useBackdropClose(isTop, close);
  if (!open) return null;
  return (
    <Portal>
      <GuardedClose value={close}>
        <div className="fixed inset-0 z-50">
          <div className="absolute inset-0 bg-overlay" aria-hidden="true" {...backdrop} />
          <div
            ref={ref}
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
            tabIndex={-1}
            className={cn(
              'absolute top-0 right-0 bottom-0 flex w-full animate-slide-in flex-col border-l border-line-strong bg-raised shadow-pop',
              width === 'lg' ? 'max-w-[760px]' : 'max-w-[560px]',
            )}
          >
            <div className="flex items-start justify-between gap-3 border-b border-line px-5 py-4">
              <div className="min-w-0">
                <h2 id={titleId} className="truncate text-title font-semibold text-fg">
                  {title}
                </h2>
                {subtitle ? <div className="mt-1 text-ui text-muted">{subtitle}</div> : null}
              </div>
              <div className="flex items-center gap-1">
                {headerActions}
                <IconButton icon="x" label="Close" onClick={close} />
              </div>
            </div>
            <div className="flex-1 overflow-y-auto px-5 py-4">{children}</div>
            {footer ? (
              <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3">
                {footer}
              </div>
            ) : null}
          </div>
          {asking ? (
            <DiscardChanges
              onKeep={() => setAsking(false)}
              onDiscard={() => {
                setAsking(false);
                onClose();
              }}
            />
          ) : null}
        </div>
      </GuardedClose>
    </Portal>
  );
}

export interface PopoverProps {
  open: boolean;
  onClose: () => void;
  anchor: RefObject<HTMLElement | null>;
  children: ReactNode;
  align?: 'left' | 'right';
  side?: 'bottom' | 'top';
  className?: string;
  label: string;
  role?: 'menu' | 'dialog' | 'listbox';
}

/**
 * A dropdown anchored to its trigger. It floats over everything (it flips above the trigger near
 * the bottom of the screen), closes on an outside click and on Escape, and Tab past its last
 * control closes it and continues from the trigger. A menu focuses its first item on open; focus
 * goes back to the trigger when it closes from inside.
 */
export function Popover({
  open,
  onClose,
  anchor,
  children,
  align = 'left',
  side = 'bottom',
  className,
  label,
  role = 'dialog',
}: PopoverProps) {
  const ref = useRef<HTMLDivElement>(null);
  const close = useEffectEvent(onClose);
  const isTop = useLayer(open);
  const [target, marker] = usePortalTarget();
  const mounted = open && target !== null;
  const hadFocus = useRef(false);
  useFloating(anchor, ref, mounted, { side, align: align === 'right' ? 'end' : 'start' });

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (ref.current?.contains(t) || anchor.current?.contains(t) || !isTop()) return;
      close();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key !== 'Escape' || !isTop()) return;
      e.stopPropagation();
      close();
      anchor.current?.focus();
    }
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey, true);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey, true);
    };
  }, [open, anchor, isTop]);

  useEffect(() => {
    if (!mounted || role !== 'menu') return;
    ref.current?.querySelector<HTMLElement>('[role=menuitem]:not([disabled])')?.focus();
  }, [mounted, role]);

  useLayoutEffect(() => {
    if (!open) return;
    hadFocus.current = false;
    const trigger = anchor.current;
    return () => {
      if (hadFocus.current && (document.activeElement === document.body || !document.activeElement))
        trigger?.focus();
    };
  }, [open, anchor]);

  if (!mounted) return <span ref={marker} hidden />;
  return (
    <>
      <span ref={marker} hidden />
      <Portal target={target}>
        <div
          ref={ref}
          role={role}
          aria-label={label}
          onFocus={() => {
            hadFocus.current = true;
          }}
          onKeyDown={(e) => {
            if (e.key !== 'Tab') return;
            const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
              (el) => el.offsetParent !== null || el === document.activeElement,
            );
            const first = items[0];
            const last = items[items.length - 1];
            const leaving =
              role === 'menu' ||
              items.length === 0 ||
              (e.shiftKey ? document.activeElement === first : document.activeElement === last);
            if (!leaving) return;
            // Focus the trigger, then let Tab move on from it (Shift+Tab stops on the trigger).
            hadFocus.current = false;
            if (e.shiftKey) e.preventDefault();
            anchor.current?.focus();
            onClose();
          }}
          className={cn(
            'fixed top-0 left-0 z-50 max-h-[var(--floating-max-height,80vh)] animate-fade-in overflow-x-hidden overflow-y-auto rounded-container border border-line-strong bg-raised shadow-pop',
            className,
          )}
        >
          {children}
        </div>
      </Portal>
    </>
  );
}

/** Moves focus between `[role=menuitem]` children with the arrow keys. */
export function menuKeyHandler(e: React.KeyboardEvent<HTMLElement>): void {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Home' && e.key !== 'End') return;
  const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>('[role=menuitem],[role=option]'));
  if (items.length === 0) return;
  e.preventDefault();
  const i = items.indexOf(document.activeElement as HTMLElement);
  let next = 0;
  if (e.key === 'ArrowDown') next = (i + 1) % items.length;
  else if (e.key === 'ArrowUp') next = (i - 1 + items.length) % items.length;
  else if (e.key === 'End') next = items.length - 1;
  items[next]?.focus();
}
