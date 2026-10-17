'use client';

import { usePathname } from 'next/navigation';
import { Children, useRef, type KeyboardEvent, type ReactNode } from 'react';

import { cn } from '@/lib/cn';
import { activeNav, navItem } from '@/lib/nav';

import { Dot, type Tone } from './badge';
import { FavouriteButton } from './favourite';
import { Icon, type IconName } from './icon';
import { Tooltip } from './tooltip';

export interface Crumb {
  label: string;
  href?: string;
}

export interface PageHeaderProps {
  title: string;
  /** One line on what the page is for, in plain words ("Live traffic, spend and health"). */
  description?: ReactNode;
  /** What the page is showing now, after the description ("Last 6 hours"). */
  context?: ReactNode;
  /**
   * Short facts after the title ("11 live keys · 3 rotation overdue"). Prefer `description`; when
   * a summary strip shows the same figures, drop it.
   */
  meta?: ReactNode;
  /** The trail above the title on a detail page ("API keys / support-bot"). */
  breadcrumb?: Crumb[];
  /**
   * Right-aligned actions: at most one `primary` button (the action the page is for), the rest
   * secondary or ghost; destructive and rare actions belong in a menu or a danger zone.
   */
  actions?: ReactNode;
}

/**
 * The page's header row, as in the prototype: "Title · purpose · context" on one line, actions on
 * the right. The title keeps at least 24rem: actions that leave it less wrap under it instead of
 * squeezing it. Under 768px the purpose and context go under the title, and under 640px each fact
 * takes its own line.
 */
export function PageHeader({ title, description, context, meta, breadcrumb, actions }: PageHeaderProps) {
  const facts = [description, meta, context].filter(
    (f) => f !== undefined && f !== null && f !== false && f !== '',
  );
  return (
    <header className="flex min-h-14 flex-wrap items-center gap-x-4 gap-y-2 border-b border-line bg-raised px-gutter py-3 max-sm:px-gutter-sm">
      <div className="min-w-0 flex-[1_1_24rem] max-md:basis-full">
        {breadcrumb && breadcrumb.length > 0 ? (
          <nav aria-label="Breadcrumb" className="mb-0.5">
            <ol className="flex flex-wrap items-center gap-1 text-caption text-muted">
              {breadcrumb.map((c, i) => (
                <li key={`${c.label}-${i}`} className="flex items-center gap-1">
                  {c.href ? (
                    <a href={c.href} className="hover:text-fg hover:underline">
                      {c.label}
                    </a>
                  ) : (
                    <span>{c.label}</span>
                  )}
                  <Icon name="chevronRight" size={12} className="text-dim" />
                </li>
              ))}
            </ol>
          </nav>
        ) : null}
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
          <h1 className="text-page font-semibold tracking-[-0.01em] text-fg">{title}</h1>
          <PageStar />
          {facts.length > 0 ? (
            <>
              <span aria-hidden="true" className="text-ui text-dim max-md:hidden">
                ·
              </span>
              <p className="min-w-0 text-ui text-muted max-md:basis-full">
                {facts.map((f, i) => (
                  <span key={i} className="inline-block max-sm:block">
                    {i > 0 ? <span className="px-2 text-dim max-sm:hidden">·</span> : null}
                    {f}
                  </span>
                ))}
              </p>
            </>
          ) : null}
        </div>
      </div>
      {actions ? <div className="flex flex-wrap items-center gap-2 max-sm:basis-full">{actions}</div> : null}
    </header>
  );
}

/** The star that pins the current screen to the user's favourites. */
function PageStar() {
  const pathname = usePathname();
  const item = navItem(activeNav(pathname));
  if (!item) return null;
  return (
    <FavouriteButton kind="page" name={item.href} label={`the ${item.label} page`} className="self-center" />
  );
}

export interface PanelProps {
  title?: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClassName?: string;
  as?: 'section' | 'div';
  /** The body runs edge to edge, with no padding: a table or a list whose rows span the panel. */
  flush?: boolean;
}

/**
 * A titled block, flat on the page: a header row (title, a muted subtitle, actions on the right)
 * over its body. Inside a `TileGrid` it is one tile; on its own, use `Section`, which adds the
 * hairline under it. Never a bordered, rounded card.
 */
export function Panel({
  title,
  subtitle,
  actions,
  children,
  className,
  bodyClassName,
  as = 'section',
  flush,
}: PanelProps) {
  const Tag = as;
  return (
    <Tag className={cn('flex min-w-0 flex-col bg-raised', className)}>
      {title || subtitle || actions ? (
        <div className="flex min-h-11 flex-wrap items-center justify-between gap-x-3 gap-y-1 px-5 pt-3 pb-1.5 max-sm:px-gutter-sm">
          <div className="flex min-w-0 items-baseline gap-2">
            {title ? <h2 className="shrink-0 text-ui font-semibold text-fg">{title}</h2> : null}
            {subtitle ? (
              <span className="min-w-0 text-caption text-muted sm:truncate">{subtitle}</span>
            ) : null}
          </div>
          {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
        </div>
      ) : null}
      <div className={cn('min-w-0', flush ? 'pb-0' : 'px-5 pb-4 max-sm:px-gutter-sm', bodyClassName)}>
        {children}
      </div>
    </Tag>
  );
}

/**
 * A section of a page or a dialog: flat and full-bleed, with a hairline under it, a header row
 * (title, muted subtitle, actions right) and its body. Sections stack; two side by side go in a
 * `TileGrid`, split by a vertical hairline.
 */
export function Section(props: PanelProps) {
  return <Panel {...props} className={cn('border-b border-line', props.className)} />;
}

type Breakpoint = 'base' | 'sm' | 'md' | 'lg' | 'xl' | '2xl';
/** Columns per breakpoint, from the smallest up ({ base: 1, lg: 2, xl: 3 }). */
export type TileColumns = Partial<Record<Breakpoint, 1 | 2 | 3 | 4 | 5 | 6>>;

const BREAKPOINTS: Breakpoint[] = ['base', 'sm', 'md', 'lg', 'xl', '2xl'];
// Literal class names, so Tailwind generates them.
const GRID_COLS: Record<Breakpoint, Record<number, string>> = {
  base: {
    1: 'grid-cols-1',
    2: 'grid-cols-2',
    3: 'grid-cols-3',
    4: 'grid-cols-4',
    5: 'grid-cols-5',
    6: 'grid-cols-6',
  },
  sm: {
    1: 'sm:grid-cols-1',
    2: 'sm:grid-cols-2',
    3: 'sm:grid-cols-3',
    4: 'sm:grid-cols-4',
    5: 'sm:grid-cols-5',
    6: 'sm:grid-cols-6',
  },
  md: {
    1: 'md:grid-cols-1',
    2: 'md:grid-cols-2',
    3: 'md:grid-cols-3',
    4: 'md:grid-cols-4',
    5: 'md:grid-cols-5',
    6: 'md:grid-cols-6',
  },
  lg: {
    1: 'lg:grid-cols-1',
    2: 'lg:grid-cols-2',
    3: 'lg:grid-cols-3',
    4: 'lg:grid-cols-4',
    5: 'lg:grid-cols-5',
    6: 'lg:grid-cols-6',
  },
  xl: {
    1: 'xl:grid-cols-1',
    2: 'xl:grid-cols-2',
    3: 'xl:grid-cols-3',
    4: 'xl:grid-cols-4',
    5: 'xl:grid-cols-5',
    6: 'xl:grid-cols-6',
  },
  '2xl': {
    1: '2xl:grid-cols-1',
    2: '2xl:grid-cols-2',
    3: '2xl:grid-cols-3',
    4: '2xl:grid-cols-4',
    5: '2xl:grid-cols-5',
    6: '2xl:grid-cols-6',
  },
};
const SHOW: Record<Breakpoint, string> = {
  base: 'block',
  sm: 'sm:block',
  md: 'md:block',
  lg: 'lg:block',
  xl: 'xl:block',
  '2xl': '2xl:block',
};
const HIDE: Record<Breakpoint, string> = {
  base: 'hidden',
  sm: 'sm:hidden',
  md: 'md:hidden',
  lg: 'lg:hidden',
  xl: 'xl:hidden',
  '2xl': '2xl:hidden',
};

/** For each breakpoint the grid changes at, how many empty cells finish a short last row. */
export function fillerCounts(count: number, cols: TileColumns): [Breakpoint, number][] {
  let current = cols.base ?? 1;
  return BREAKPOINTS.flatMap((bp) => {
    const n = cols[bp];
    if (bp !== 'base' && n === undefined) return [];
    if (n !== undefined) current = n;
    return [[bp, (current - (count % current)) % current] as [Breakpoint, number]];
  });
}

/** The classes of the i-th filler cell: shown at the breakpoints whose last row it completes. */
export function fillerClass(i: number, counts: [Breakpoint, number][]): string {
  let shown: boolean | null = null;
  const out: string[] = [];
  for (const [bp, n] of counts) {
    const show = i < n;
    if (show !== shown) out.push(show ? SHOW[bp] : HIDE[bp]);
    shown = show;
  }
  return out.join(' ');
}

export interface TileGridProps {
  children: ReactNode;
  className?: string;
  label?: string;
  /**
   * Columns per breakpoint. With it the grid sets its columns (unless `template` does) and fills a
   * short last row with empty tiles, so every tile keeps all four hairlines and no hole shows.
   */
  cols?: TileColumns;
  /** Column templates that `cols` can't say (`lg:grid-cols-[2fr_1fr_1fr]`); `cols` still counts. */
  template?: string;
  /**
   * `band` (default): full-bleed with a hairline under it. `flat`: hairlines above and below, no box,
   * for key-value facts in drawers and detail panes. `inset`: a bordered block.
   */
  variant?: 'band' | 'flat' | 'inset';
  as?: 'section' | 'div' | 'ul' | 'dl';
}

/**
 * Tiles separated by 1px hairlines: the prototype's bands of charts, figures and cards. The lines
 * are the grid's background showing through a 1px gap, so seams never double, and with `cols` the
 * empty cells of a short last row are filled with blank tiles instead of showing the line colour.
 */
export function TileGrid({
  children,
  className,
  label,
  cols,
  template,
  variant = 'band',
  as = 'section',
}: TileGridProps) {
  const Tag = as;
  const count = Children.toArray(children).length;
  const counts = cols ? fillerCounts(count, cols) : [];
  const fillers = Math.max(0, ...counts.map(([, n]) => n));
  const columnClasses =
    cols && !template
      ? BREAKPOINTS.map((bp) => (cols[bp] ? GRID_COLS[bp][cols[bp]] : null))
          .filter(Boolean)
          .join(' ')
      : template;
  const Filler = as === 'ul' ? 'li' : 'div';
  return (
    <Tag
      aria-label={label}
      className={cn(
        'grid gap-px bg-line',
        variant === 'band'
          ? 'border-b border-line'
          : variant === 'flat'
            ? 'border-y border-line'
            : 'overflow-hidden rounded-control border border-line',
        columnClasses,
        className,
      )}
    >
      {children}
      {Array.from({ length: fillers }, (_, i) => (
        <Filler key={`filler-${i}`} aria-hidden="true" className={cn('bg-raised', fillerClass(i, counts))} />
      ))}
    </Tag>
  );
}

/** A small section label with an optional status dot ("● Active credentials"). */
export function Eyebrow({
  children,
  tone,
  className,
}: {
  children: ReactNode;
  tone?: Tone;
  className?: string;
}) {
  return (
    <div className={cn('eyebrow flex items-center gap-2', className)}>
      {tone ? <Dot tone={tone} /> : null}
      {children}
    </div>
  );
}

export interface StatProps {
  label: ReactNode;
  value: ReactNode;
  unit?: ReactNode;
  sub?: ReactNode;
  tone?: Tone;
  children?: ReactNode;
  className?: string;
}

const VALUE_TONE: Record<Tone, string> = {
  neutral: 'text-fg',
  success: 'text-success',
  warn: 'text-warn',
  error: 'text-error',
  info: 'text-info',
  accent: 'text-accent',
};

/** A KPI: label, value with unit, a sub line, and optionally a sparkline below. */
export function Stat({ label, value, unit, sub, tone = 'neutral', children, className }: StatProps) {
  return (
    <div className={cn('min-w-0', className)}>
      <div className="eyebrow">{label}</div>
      <div
        className={cn(
          'mono mt-1.5 text-kpi font-semibold tracking-tight whitespace-nowrap tabular-nums',
          VALUE_TONE[tone],
        )}
      >
        {value}
        {unit ? <span className="font-sans text-caption font-normal text-muted"> {unit}</span> : null}
      </div>
      {sub ? <div className="mt-1 text-caption text-muted">{sub}</div> : null}
      {children ? <div className="mt-2">{children}</div> : null}
    </div>
  );
}

export interface SummaryItem {
  /** Sentence case, with the window when it has one ("Requests, last 24 h"). */
  label: string;
  value: ReactNode;
  tone?: Tone;
  /** Makes the figure a filter link ("3 need rotation" → shows those keys). */
  onClick?: (() => void) | undefined;
  /** The tooltip, such as how the figure is counted. */
  title?: string | undefined;
  /** One short line under the figure: a trend, a breakdown or a sparkline. */
  sub?: ReactNode;
}

/**
 * The compact summary over a list (≤ 72px): three to five figures in one row, each optionally a
 * filter. It replaces the tall hero on list pages; hide it when every figure is zero or unknown.
 */
export function SummaryStrip({ items, label = 'Summary' }: { items: SummaryItem[]; label?: string }) {
  return (
    <section
      aria-label={label}
      className="flex flex-wrap items-stretch gap-px border-b border-line bg-line [&>*]:flex-1 [&>*]:basis-[160px]"
    >
      {items.map((item) => {
        const body = (
          <>
            <span className="block text-caption text-muted">{item.label}</span>
            <span
              className={cn(
                'mono mt-0.5 block text-[20px] leading-7 font-semibold tracking-[-0.01em] whitespace-nowrap tabular-nums',
                VALUE_TONE[item.tone ?? 'neutral'],
              )}
            >
              {item.value}
            </span>
            {item.sub ? (
              <span className="mt-0.5 block truncate text-caption text-muted">{item.sub}</span>
            ) : null}
          </>
        );
        return item.onClick ? (
          <Tooltip key={item.label} content={item.title}>
            <button
              type="button"
              onClick={item.onClick}
              className="flex min-w-0 flex-col justify-start bg-raised px-gutter py-2.5 text-left hover:bg-hover max-sm:px-gutter-sm"
            >
              {body}
            </button>
          </Tooltip>
        ) : (
          <Tooltip key={item.label} content={item.title}>
            <div className="min-w-0 bg-raised px-gutter py-2.5 max-sm:px-gutter-sm">{body}</div>
          </Tooltip>
        );
      })}
    </section>
  );
}

/** The hero band: a large lead card, a grid of KPI cells and a side panel. */
export function Hero({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('grid gap-px border-b border-line bg-line', className)}>{children}</div>;
}

export function HeroCell({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('min-w-0 bg-raised px-6 py-5', className)}>{children}</div>;
}

export interface HeroLeadProps {
  eyebrow: ReactNode;
  tone?: Tone;
  value: ReactNode;
  unit?: ReactNode;
  children?: ReactNode;
  className?: string;
}

/** The hero's lead figure ("9 keys serving traffic"), on a tinted background. */
export function HeroLead({ eyebrow, tone = 'accent', value, unit, children, className }: HeroLeadProps) {
  return (
    <div
      className={cn(
        'flex min-w-0 flex-col gap-4 bg-raised bg-[linear-gradient(135deg,color-mix(in_oklab,var(--ant-accent)_9%,transparent),transparent_60%)] px-6 py-5',
        className,
      )}
    >
      <Eyebrow tone={tone}>{eyebrow}</Eyebrow>
      <div className="flex items-baseline gap-3">
        <span className="mono text-hero leading-none font-semibold tracking-tighter text-fg tabular-nums">
          {value}
        </span>
        {unit ? <span className="text-body text-muted">{unit}</span> : null}
      </div>
      {children}
    </div>
  );
}

/** A bullet list of facts under a hero figure ("■ 1 rotating · grace window open"). */
export function FactList({ items }: { items: { tone: Tone; value: ReactNode; label: ReactNode }[] }) {
  return (
    <ul className="flex flex-col gap-1.5">
      {items.map((item, i) => (
        <li key={i} className="flex items-center gap-2 text-ui text-muted">
          <Dot tone={item.tone} className="rounded-[1px]" />
          <span className="mono font-semibold text-fg tabular-nums">{item.value}</span> {item.label}
        </li>
      ))}
    </ul>
  );
}

export interface TabItem<T extends string> {
  value: T;
  label: string;
  count?: number | undefined;
  icon?: IconName;
}

export interface TabsProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  items: TabItem<T>[];
  label: string;
  className?: string;
  /** Prefix for the `id` of each tab, so panels can reference it with `aria-labelledby`. */
  idPrefix?: string;
}

/** Underlined tabs with arrow-key movement (WAI-ARIA tabs pattern). */
export function Tabs<T extends string>({
  value,
  onChange,
  items,
  label,
  className,
  idPrefix = 'tab',
}: TabsProps<T>) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  function onKey(e: KeyboardEvent<HTMLDivElement>) {
    const i = items.findIndex((t) => t.value === value);
    let next = -1;
    if (e.key === 'ArrowRight') next = (i + 1) % items.length;
    else if (e.key === 'ArrowLeft') next = (i - 1 + items.length) % items.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    if (next < 0) return;
    e.preventDefault();
    const item = items[next];
    if (item) {
      onChange(item.value);
      refs.current[next]?.focus();
    }
  }
  return (
    <div
      role="tablist"
      aria-label={label}
      onKeyDown={onKey}
      className={cn(
        'flex items-end gap-1 overflow-x-auto border-b border-line bg-raised px-gutter max-sm:px-gutter-sm',
        className,
      )}
    >
      {items.map((t, i) => {
        const on = t.value === value;
        return (
          <button
            key={t.value}
            ref={(el) => {
              refs.current[i] = el;
            }}
            id={`${idPrefix}-${t.value}`}
            role="tab"
            type="button"
            aria-selected={on}
            tabIndex={on ? 0 : -1}
            onClick={() => onChange(t.value)}
            className={cn(
              '-mb-px inline-flex shrink-0 items-center gap-2 border-b-2 px-3 py-2.5 text-body whitespace-nowrap transition-colors',
              on ? 'border-accent font-semibold text-fg' : 'border-transparent text-muted hover:text-fg',
            )}
          >
            {t.icon ? <Icon name={t.icon} size={14} /> : null}
            {t.label}
            {t.count !== undefined ? (
              <span className="text-caption text-dim tabular-nums">{t.count}</span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

/** The filter row under tabs: labelled groups of controls, wrapping on narrow screens. */
export function FilterBar({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        'flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-line bg-canvas px-gutter py-2.5 max-sm:px-gutter-sm',
        className,
      )}
    >
      {children}
    </div>
  );
}

export function FilterGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2" role="group" aria-label={label}>
      <span className="text-caption text-muted">{label}</span>
      {children}
    </div>
  );
}

/** A key-value grid for detail panes and drawers: flat, divided by hairlines, never a box. */
export function DefinitionGrid({
  items,
  columns = 2,
}: {
  items: { label: ReactNode; value: ReactNode }[];
  columns?: 2 | 3 | 4;
}) {
  return (
    <TileGrid as="dl" variant="flat" cols={{ base: 1, sm: columns }}>
      {items.map((item, i) => (
        <div key={i} className="min-w-0 bg-raised px-3.5 py-2.5">
          <dt className="eyebrow">{item.label}</dt>
          <dd className="mono mt-1 truncate text-body text-fg tabular-nums">{item.value}</dd>
        </div>
      ))}
    </TileGrid>
  );
}
