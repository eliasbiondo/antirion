'use client';

import Link from 'next/link';
import { type ReactNode, useState } from 'react';

import { ApiError, describeError, isChangeControl } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import { useUi } from '@/stores/ui';

import { Button } from './button';
import { Icon, type IconName } from './icon';
import { Tooltip } from './tooltip';

export function Skeleton({ className }: { className?: string }) {
  return <div aria-hidden="true" className={cn('animate-pulse rounded-sm bg-hover', className)} />;
}

/** Rows of placeholders while a list loads; the page keeps its shape. */
export function LoadingRows({ rows = 6, label = 'Loading' }: { rows?: number; label?: string }) {
  return (
    <div role="status" aria-live="polite" className="flex flex-col">
      <span className="sr-only">{label}…</span>
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="flex h-[var(--spacing-row)] items-center gap-4 border-b border-line bg-raised px-gutter"
        >
          <Skeleton className="h-3 w-40" />
          <Skeleton className="h-3 w-24" />
          <Skeleton className="ml-auto h-3 w-16" />
        </div>
      ))}
    </div>
  );
}

/** "Alerts" → "alerts", but "SCIM status" stays: lower-cases a subject for use mid-sentence. */
export function midSentence(subject: string): string {
  const second = subject.charAt(1);
  const lowerSecond = second !== '' && second === second.toLowerCase();
  return lowerSecond ? subject.charAt(0).toLowerCase() + subject.slice(1) : subject;
}

/** The empty state's one visual: a small bordered pill with muted text ("No requests in this range"). */
export function EmptyPill({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        'inline-block max-w-full rounded-control border border-line bg-raised px-2.5 py-1 text-center text-caption text-balance text-muted',
        className,
      )}
    >
      {children}
    </span>
  );
}

/** Vertical room around a state: `sm` inside a chart, card or drawer; `md` in place of a page's table. */
const STATE_PADDING = { sm: 'py-6', md: 'py-16' } as const;

const QUIET_ACTION =
  'inline-flex h-7 items-center rounded-control px-2 text-caption font-medium text-accent hover:bg-hover hover:underline underline-offset-2 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:no-underline';

export interface EmptyActionProps {
  children: ReactNode;
  onClick?: () => void;
  /** A link instead of a button (a page of the console). */
  href?: string;
  disabled?: boolean;
  /** Set by `Gate mode="disable"`: why the action can't be used. */
  disabledReason?: string;
}

/** The quiet action under an empty pill ("Show last 24 hours", "Clear filters", "Create rule"). */
export function EmptyAction({ children, onClick, href, disabled, disabledReason }: EmptyActionProps) {
  if (href && !disabled) {
    return (
      <Link href={href} className={QUIET_ACTION}>
        {children}
      </Link>
    );
  }
  const button = (
    <button type="button" onClick={onClick} disabled={disabled} className={QUIET_ACTION}>
      {children}
    </button>
  );
  return disabled && disabledReason ? (
    <Tooltip content={disabledReason}>
      <span className="inline-flex">{button}</span>
    </Tooltip>
  ) : (
    button
  );
}

export interface EmptyStateProps {
  /** The pill: what is empty, in a few words ("No API keys yet"). */
  title: string;
  /** At most one short muted line under the pill: why it is empty, or how to fill it. */
  description?: ReactNode;
  /** The action that fills the view, as an `EmptyAction` ("Create key"). */
  action?: ReactNode;
  /** A second `EmptyAction`, beside the first. */
  secondaryAction?: ReactNode;
  /** `sm` inside a chart, card, drawer or tab panel; `md` (default) in place of a page's table. */
  size?: 'sm' | 'md';
  className?: string;
}

/**
 * The console's one empty state: a small bordered pill with muted text, centred where the content
 * would be, with an optional muted line and quiet actions under it. No icon, no headline: the
 * page keeps its header, filters and columns, so the pill only has to say what is missing.
 */
export function EmptyState({
  title,
  description,
  action,
  secondaryAction,
  size = 'md',
  className,
}: EmptyStateProps) {
  return (
    <div
      data-empty-state=""
      className={cn(
        'flex flex-col items-center justify-center gap-2 px-4 text-center',
        STATE_PADDING[size],
        className,
      )}
    >
      <EmptyPill>{title}</EmptyPill>
      {description ? <p className="max-w-md text-caption text-balance text-muted">{description}</p> : null}
      {action || secondaryAction ? (
        <div className="flex flex-wrap items-center justify-center gap-x-1 gap-y-1">
          {action}
          {secondaryAction}
        </div>
      ) : null}
    </div>
  );
}

/** "No results match these filters" with the clear-filters action every list offers. */
export function NoMatch({ onClear, size }: { onClear: () => void; size?: 'sm' | 'md' }) {
  return (
    <EmptyState
      size={size}
      title="No results match these filters"
      action={<EmptyAction onClick={onClear}>Clear filters</EmptyAction>}
    />
  );
}

/**
 * Whether a failed read means "this gateway does not serve the endpoint". Only a read of a fixed
 * path says so: a 404 on a path that names an entity (`GET /logs/{id}`) means that entity is not
 * there, which an older gateway and a current one answer alike.
 */
export function isUnavailable(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404 && !namesAnEntity(error.operation);
}

/** Whether a failed read was of one entity: a 404 then means it does not exist. */
export function isNotFound(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404 && namesAnEntity(error.operation);
}

function namesAnEntity(operation: string): boolean {
  return /\{[^}]+\}/.test(operation);
}

/** The operator-facing facts of a failure, behind a "Details" disclosure. */
function ErrorDetails({ error, extra }: { error: unknown; extra?: string | undefined }) {
  const message = error instanceof ApiError ? error.message : error instanceof Error ? error.message : null;
  const requestId = error instanceof ApiError ? error.requestId : null;
  if (!message && !requestId && !extra) return null;
  return (
    <details className="group w-full max-w-md text-left">
      <summary className="mx-auto flex w-fit cursor-pointer list-none items-center gap-1 rounded-xs text-caption text-muted hover:text-fg [&::-webkit-details-marker]:hidden">
        <Icon name="chevronRight" size={12} className="transition-transform group-open:rotate-90" />
        Details
      </summary>
      <dl className="mt-2 flex flex-col gap-1 rounded-control border border-line bg-sunken px-3 py-2 text-caption">
        {extra ? (
          <div className="flex gap-2">
            <dt className="shrink-0 text-dim">Endpoint</dt>
            <dd className="mono min-w-0 break-all text-muted">{extra}</dd>
          </div>
        ) : null}
        {message ? (
          <div className="flex gap-2">
            <dt className="shrink-0 text-dim">Message</dt>
            <dd className="min-w-0 break-words text-muted">{message}</dd>
          </div>
        ) : null}
        {requestId ? (
          <div className="flex gap-2">
            <dt className="shrink-0 text-dim">Request ID</dt>
            <dd className="mono min-w-0 break-all text-muted">{requestId}</dd>
          </div>
        ) : null}
      </dl>
    </details>
  );
}

export interface UnavailableStateProps {
  /** What the view shows ("Analytics", "Alerts"). */
  subject: string;
  /** What still works, in one sentence ("Recent requests still appear below."). */
  description?: ReactNode;
  /** The contract operation, shown to operators behind "Details". */
  operation?: string | undefined;
  size?: 'sm' | 'md';
  className?: string;
}

/** A capability the gateway does not serve yet: said once, in plain words, never as zeros. */
export function UnavailableState({
  subject,
  description,
  operation,
  size = 'md',
  className,
}: UnavailableStateProps) {
  return (
    <div
      data-empty-state=""
      className={cn(
        'flex flex-col items-center justify-center gap-2 px-4 text-center',
        STATE_PADDING[size],
        className,
      )}
    >
      <EmptyPill>{`${subject} isn't available on this gateway yet`}</EmptyPill>
      <p className="max-w-md text-caption text-balance text-muted">
        {description ?? 'Upgrade the gateway to use it.'}
      </p>
      {operation ? <ErrorDetails error={null} extra={operation} /> : null}
    </div>
  );
}

export interface ErrorStateProps {
  error: unknown;
  onRetry?: () => void;
  /** What the view shows, for the wording ("Alerts", "The request log"). */
  subject?: string;
  size?: 'sm' | 'md';
  className?: string;
}

/**
 * A failed read, shown in place: 403 as "you don't have access", 404 on a fixed path as "not
 * available on this gateway yet" (the endpoint is in the contract but this gateway does not serve
 * it), 404 on an entity as "couldn't find", and anything else as "couldn't load", with the message
 * and request id behind "Details".
 */
export function ErrorState({ error, onRetry, subject, size = 'md', className }: ErrorStateProps) {
  if (error instanceof ApiError && error.status === 403) {
    return (
      <EmptyState
        size={size}
        className={className}
        title={`You don't have access to ${subject ? midSentence(subject) : 'this'}`}
        description="Ask a gateway admin for access."
      />
    );
  }
  if (subject && isUnavailable(error)) {
    return (
      <UnavailableState
        subject={subject}
        operation={error instanceof ApiError ? error.operation : undefined}
        size={size}
        className={className}
      />
    );
  }
  if (error instanceof ApiError && error.status === 404) {
    return (
      <EmptyState
        size={size}
        className={className}
        title={`We couldn't find ${subject ? midSentence(subject) : 'this'}`}
        description="It may have been deleted, or the link has a mistake in it."
      />
    );
  }
  // A failure keeps the empty state's shape but not its look: an error-toned pill with its icon.
  return (
    <div
      role="alert"
      className={cn(
        'flex flex-col items-center justify-center gap-2 px-4 text-center',
        STATE_PADDING[size],
        className,
      )}
    >
      <span className="inline-flex max-w-full items-center gap-1.5 rounded-control border border-[color-mix(in_oklab,var(--ant-error)_45%,var(--ant-border))] bg-raised px-2.5 py-1 text-caption text-fg">
        <Icon name="alertCircle" size={14} className="shrink-0 text-error" />
        Couldn&apos;t load {subject ? midSentence(subject) : 'this view'}
      </span>
      <p className="max-w-md text-caption text-balance text-muted">
        Something went wrong on the gateway. Try again, or share the details with your admin.
      </p>
      <ErrorDetails error={error} />
      {onRetry ? <EmptyAction onClick={onRetry}>Try again</EmptyAction> : null}
    </div>
  );
}

/** An inline banner for a form or a failed write: what happened and what to do. */
/** The request id of a failed call, copied on demand for a support ticket, not shown as raw text. */
function CopyRequestId({ id }: { id: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="mt-1 rounded-xs text-caption text-dim hover:text-fg"
      onClick={() => {
        void navigator.clipboard.writeText(id).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      {copied ? 'Request ID copied' : 'Copy request ID'}
    </button>
  );
}

/**
 * A failed write, in words, under the form. With `onReload`, a conflict (someone else saved first)
 * offers Reload, which reads their version and keeps the form's edits.
 */
export function InlineError({ error, onReload }: { error: unknown; onReload?: () => void }) {
  if (!error) return null;
  if (isChangeControl(error)) return <ChangeControlNote error={error} />;
  return (
    <div
      role="alert"
      className="flex items-start gap-2.5 rounded-control border border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_8%,transparent)] px-3 py-2.5 text-ui text-fg"
    >
      <span className="mt-px text-error">
        <Icon name="alertCircle" size={16} />
      </span>
      <div className="min-w-0 flex-1">
        <p>{describeError(error)}</p>
        {error instanceof ApiError && error.details.length > 1 ? (
          <ul className="mt-1 list-disc pl-4 text-caption text-muted">
            {error.details.map((d, i) => (
              <li key={i}>
                <span className="mono">{d.path}</span>: {d.message}
              </li>
            ))}
          </ul>
        ) : null}
        {error instanceof ApiError && error.requestId ? <CopyRequestId id={error.requestId} /> : null}
      </div>
      {onReload && error instanceof ApiError && error.preconditionFailed ? (
        <Button size="sm" onClick={onReload}>
          Reload
        </Button>
      ) : null}
    </div>
  );
}

const BANNER_ICON: Record<'info' | 'warn' | 'error' | 'success', IconName> = {
  info: 'info',
  warn: 'alert',
  error: 'alertCircle',
  success: 'checkCircle',
};

/**
 * A full-width note under the page header: a capability that is missing, a state worth knowing.
 * Title and one sentence, with actions on the right.
 */
export function Banner({
  tone,
  title,
  children,
  actions,
}: {
  tone: 'info' | 'warn' | 'error' | 'success';
  title: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
}) {
  const text = { info: 'text-info', warn: 'text-warn', error: 'text-error', success: 'text-success' }[tone];
  return (
    <div
      className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-line bg-raised px-gutter py-3 max-sm:px-gutter-sm"
      role={tone === 'error' ? 'alert' : 'status'}
    >
      <span className={cn('shrink-0 self-start pt-0.5', text)} aria-hidden="true">
        <Icon name={BANNER_ICON[tone]} size={16} />
      </span>
      <div className="min-w-0 flex-1">
        <div className="text-ui font-semibold text-fg">{title}</div>
        {children ? <div className="mt-0.5 text-ui text-muted">{children}</div> : null}
      </div>
      {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
    </div>
  );
}

/** In place of an error: the write went to review instead (a change request offered or opened). */
function ChangeControlNote({ error }: { error: ApiError }) {
  const setDraft = useUi((s) => s.setChangeDraft);
  const request = error.changeRequest;
  return (
    <div
      role="status"
      className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-control border border-[color-mix(in_oklab,var(--ant-info)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-info)_8%,transparent)] px-3 py-2.5 text-ui text-fg"
    >
      <span className="text-info" aria-hidden="true">
        <Icon name="pullRequest" size={16} />
      </span>
      <p className="min-w-0 flex-1">{describeError(error)}</p>
      {request ? (
        <Link
          href={`/change-requests/?request=${encodeURIComponent(request.id)}`}
          className="text-ui font-medium text-accent hover:underline"
        >
          View request
        </Link>
      ) : error.attempt ? (
        <button
          type="button"
          onClick={() => setDraft(error.attempt)}
          className="text-ui font-medium text-accent hover:underline"
        >
          Open change request
        </button>
      ) : null}
    </div>
  );
}
