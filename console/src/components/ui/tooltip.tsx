'use client';

import {
  isValidElement,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type ElementType,
  type FocusEvent,
  type PointerEvent,
  type ReactElement,
  type ReactNode,
  type Ref,
} from 'react';
import { createPortal } from 'react-dom';

import { cn } from '@/lib/cn';

import { useFloating } from './floating';

export interface TooltipProps {
  /** The hint; nothing (null, undefined or '') shows the trigger alone. */
  content: ReactNode;
  /** One element that takes a ref and the pointer and focus handlers (a button, a link, a span). */
  children: ReactElement;
  side?: 'top' | 'bottom';
  /** The hover delay in ms; keyboard focus shows it at once. */
  delay?: number;
  /**
   * Whether the trigger is described by the tooltip (`aria-describedby`). Turn it off when the
   * content repeats the trigger's accessible name, as an icon button's label does.
   */
  describe?: boolean;
  className?: string;
}

type TriggerProps = Record<string, unknown> & {
  ref?: Ref<HTMLElement>;
  'aria-describedby'?: string;
  onPointerEnter?: (e: PointerEvent<HTMLElement>) => void;
  onPointerLeave?: (e: PointerEvent<HTMLElement>) => void;
  onFocus?: (e: FocusEvent<HTMLElement>) => void;
  onBlur?: (e: FocusEvent<HTMLElement>) => void;
};

function assign(ref: Ref<HTMLElement> | undefined, node: HTMLElement | null) {
  if (typeof ref === 'function') ref(node);
  else if (ref) ref.current = node;
}

function focusVisible(el: Element): boolean {
  try {
    return el.matches(':focus-visible');
  } catch {
    return true;
  }
}

/** Where the tooltip mounts: in the modal dialog that holds the trigger, else the body. */
function targetFor(el: HTMLElement | null): HTMLElement | null {
  if (!el) return null;
  return el.closest<HTMLElement>('[aria-modal="true"]') ?? el.ownerDocument.body;
}

/**
 * The console's tooltip, instead of the browser's `title`: it shows after a short hover or at once
 * on keyboard focus, hides on leave, blur, a press and Escape, describes its trigger
 * (`aria-describedby`), and floats on the side with room. For a control that can be disabled
 * (a disabled button gets no pointer events), wrap it in a span and put the tooltip on the span.
 */
export function Tooltip({
  content,
  children,
  side = 'top',
  delay = 500,
  describe = true,
  className,
}: TooltipProps) {
  const id = useId();
  const anchor = useRef<HTMLElement | null>(null);
  const tip = useRef<HTMLDivElement>(null);
  const timer = useRef<number | undefined>(undefined);
  const [target, setTarget] = useState<HTMLElement | null>(null);
  const open = target !== null;
  useFloating(anchor, tip, open, { side, align: 'center', offset: 6 });

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // The first Escape only dismisses the tooltip, as the pattern asks; the next one reaches
      // the dialog or menu underneath.
      e.stopPropagation();
      e.preventDefault();
      setTarget(null);
    };
    const onDown = () => setTarget(null);
    window.addEventListener('keydown', onKey, true);
    document.addEventListener('pointerdown', onDown, true);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      document.removeEventListener('pointerdown', onDown, true);
    };
  }, [open]);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const childRef = isValidElement(children) ? (children.props as TriggerProps).ref : undefined;
  const setAnchor = useCallback(
    (node: HTMLElement | null) => {
      anchor.current = node;
      assign(childRef, node);
    },
    [childRef],
  );

  const empty = content === null || content === undefined || content === false || content === '';
  if (empty || !isValidElement(children)) return children;

  const props = children.props as TriggerProps;
  const show = (now: boolean) => {
    window.clearTimeout(timer.current);
    const run = () => setTarget(targetFor(anchor.current));
    if (now) run();
    else timer.current = window.setTimeout(run, delay);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    setTarget(null);
  };

  const Trigger = children.type as ElementType;
  const trigger = (
    <Trigger
      {...props}
      ref={setAnchor}
      onPointerEnter={(e: PointerEvent<HTMLElement>) => {
        props.onPointerEnter?.(e);
        if (e.pointerType !== 'touch') show(false);
      }}
      onPointerLeave={(e: PointerEvent<HTMLElement>) => {
        props.onPointerLeave?.(e);
        hide();
      }}
      onFocus={(e: FocusEvent<HTMLElement>) => {
        props.onFocus?.(e);
        if (focusVisible(e.target)) show(true);
      }}
      onBlur={(e: FocusEvent<HTMLElement>) => {
        props.onBlur?.(e);
        hide();
      }}
      aria-describedby={
        describe && open
          ? [props['aria-describedby'], id].filter(Boolean).join(' ')
          : props['aria-describedby']
      }
    />
  );

  return (
    <>
      {trigger}
      {target
        ? createPortal(
            <div
              ref={tip}
              id={id}
              role="tooltip"
              className={cn(
                'pointer-events-none fixed top-0 left-0 z-[70] max-w-[280px] animate-fade-in rounded-sm bg-fg px-2 py-1 text-caption font-medium text-canvas shadow-pop',
                className,
              )}
            >
              {content}
            </div>,
            target,
          )
        : null}
    </>
  );
}
