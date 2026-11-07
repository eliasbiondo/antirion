'use client';

import Link from 'next/link';
import { useEffect, useState } from 'react';

import { useSettingsHref } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { Section } from '@/components/ui/layout';
import { EmptyState, InlineError, LoadError, LoadingRows } from '@/components/ui/states';
import { downloadText } from '@/lib/csv';
import { notify } from '@/stores/ui';

import { type ConvergenceState, useConfigDoc, useGatewaySettings } from '../store';
import { useReportUnsaved } from '../unsaved';
import { ChangeRow } from './change-row';
import { VersionsPanel } from './versions-panel';
import { translateNow } from '@/lib/i18n';

/** Whether `admin.config.lock_applied` is on (a startup setting from the configuration file). */
function useAppliedLock(): boolean {
  const admin = useGatewaySettings((s) => s.admin);
  useEffect(() => {
    if (!admin.data) void useGatewaySettings.getState().loadAdmin();
  }, [admin.data]);
  return admin.data?.spec.config?.lock_applied === true;
}

/** "node-b", "node-b and node-c", "node-b, node-c and 2 more". */
function nodeList(ids: string[]): string {
  if (ids.length <= 1) return ids[0] ?? '';
  if (ids.length <= 3) return `${ids.slice(0, -1).join(', ')} and ${ids[ids.length - 1]}`;
  return `${ids.slice(0, 2).join(', ')} and ${ids.length - 2} more`;
}

/** Whether the version the last apply or rollback made runs on every node, in words. */
export function convergenceWords(c: ConvergenceState | null, propagated: boolean): string {
  switch (c?.state) {
    case 'checking':
      return 'checking every node…';
    case 'waiting':
      return c.lagging.length > 0 ? `waiting for ${nodeList(c.lagging)}` : 'reaching every node…';
    case 'converged':
      return 'live on every node';
    case 'lagging':
      return c.lagging.length > 0
        ? `still not on ${nodeList(c.lagging)} after a minute`
        : 'not on every node after a minute';
    default:
      return propagated ? 'live on every node' : 'reaching every node';
  }
}

/** The last apply or rollback made from this page, and whether every node runs it yet. */
function ApplyStatus() {
  const applied = useConfigDoc((s) => s.applied);
  const by = useConfigDoc((s) => s.appliedBy);
  const convergence = useConfigDoc((s) => s.convergence);
  const statusHref = useSettingsHref('status');
  const result = applied.data;
  if (!result || !by) return null;
  const c = convergence?.generation === result.generation ? convergence : null;
  const tone =
    c?.state === 'lagging' ? 'text-warn' : c?.state === 'converged' || !c ? 'text-success' : 'text-muted';
  return (
    <span role="status" className={`flex flex-wrap items-center gap-x-1.5 text-caption ${tone}`}>
      {c?.state === 'converged' || (!c && result.propagated) ? (
        <Icon name="check" size={14} className="shrink-0" />
      ) : null}
      <span>
        {by.kind === 'rollback'
          ? `Rolled back to version ${by.to} as version ${result.generation}`
          : `Applied as version ${result.generation}`}
        , {convergenceWords(c, result.propagated)}
      </span>
      {c?.state === 'lagging' || c?.state === 'waiting' ? (
        <Link href={statusHref} className="text-accent hover:underline">
          See the nodes
        </Link>
      ) : null}
    </span>
  );
}

/** The gateway's checks of the draft: problems that stop an apply, and warnings that don't. */
function ValidationResult() {
  const validation = useConfigDoc((s) => s.validation);
  const v = validation.data;
  if (validation.error) return <InlineError error={validation.error} />;
  if (!v) return null;
  if (v.valid && v.errors.length === 0 && v.warnings.length === 0) return null;
  return (
    <div className="mb-3 flex flex-col gap-2">
      {v.errors.length > 0 ? (
        <div
          role="alert"
          className="rounded-control border border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_8%,transparent)] px-3 py-2.5 text-ui text-fg"
        >
          <p className="flex items-center gap-2 font-medium">
            <Icon name="alertCircle" size={16} className="shrink-0 text-error" />
            {v.errors.length === 1
              ? 'One problem to fix before you can apply'
              : `${v.errors.length} problems to fix before you can apply`}
          </p>
          <ul className="mt-1.5 flex flex-col gap-1 pl-6">
            {v.errors.map((e, i) => (
              <li key={`${e.path}-${i}`} className="text-caption">
                <span className="mono text-fg">{e.path || '(document)'}</span>
                <span className="text-muted">: {e.message}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {v.warnings.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {v.warnings.map((w, i) => (
            <li key={`${w.path ?? ''}-${i}`} className="flex items-start gap-1.5 text-caption text-warn">
              <Icon name="alert" size={14} className="mt-0.5 shrink-0" />
              <span>
                {w.path ? <span className="mono">{w.path}: </span> : null}
                {w.message}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

/** The configuration document as JSON: edit it, see the diff against the running generation, apply. */
export function ConfigSection() {
  const document = useConfigDoc((s) => s.document);
  const draft = useConfigDoc((s) => s.draft);
  const parseError = useConfigDoc((s) => s.parseError);
  const diff = useConfigDoc((s) => s.diff);
  const validation = useConfigDoc((s) => s.validation);
  const applied = useConfigDoc((s) => s.applied);
  const loadDocument = useConfigDoc((s) => s.loadDocument);
  const setDraft = useConfigDoc((s) => s.setDraft);
  const runDiff = useConfigDoc((s) => s.runDiff);
  const apply = useConfigDoc((s) => s.apply);
  const [confirm, setConfirm] = useState(false);
  useReportUnsaved(document.data ? draft !== JSON.stringify(document.data.doc, null, 2) : false);

  useEffect(() => {
    void loadDocument();
  }, [loadDocument]);
  const nodeHref = useSettingsHref('node');
  const locked = useAppliedLock();

  const changes = diff.data?.changes ?? [];
  const invalid = (validation.data?.errors.length ?? 0) > 0;
  // The review's own warnings show above; the diff's are listed only when they say something else.
  const checked = new Set((validation.data?.warnings ?? []).map((w) => w.message));
  const diffWarnings = (diff.data?.warnings ?? [])
    .map((w) => (typeof w === 'string' ? w : ((w as { message?: string }).message ?? JSON.stringify(w))))
    .filter((m) => !checked.has(m));
  return (
    <div className="flex flex-col">
      {locked ? (
        <p className="flex items-start gap-2 border-b border-line bg-raised px-5 py-3 text-ui text-fg max-sm:px-gutter-sm">
          <Icon name="lock" size={16} className="mt-0.5 shrink-0 text-muted" />
          <span>
            Settings applied from the configuration file are locked: changes to them here are refused. Change
            them in the file instead.
          </span>
        </p>
      ) : null}
      <Section
        title="Document"
        subtitle={
          document.data?.etag
            ? `Version ${document.data.etag.replace(/"/g, '')} · Secrets are hidden and stay unchanged when you apply.`
            : undefined
        }
        actions={
          <>
            <Button size="sm" icon="refresh" variant="ghost" onClick={() => void loadDocument()}>
              Revert to current
            </Button>
            <Button
              size="sm"
              icon="download"
              variant="ghost"
              onClick={() => downloadText('antirion-config.json', draft, 'application/json')}
            >
              Download
            </Button>
          </>
        }
      >
        {document.status === 'loading' && !document.data ? <LoadingRows rows={6} /> : null}
        <LoadError of={document} subject="The configuration" onRetry={() => void loadDocument()} />
        {document.data ? (
          <>
            <label htmlFor="config-editor" className="sr-only">
              The configuration as JSON
            </label>
            <textarea
              id="config-editor"
              spellCheck={false}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              aria-invalid={parseError ? true : undefined}
              aria-describedby={parseError ? 'config-parse-error' : undefined}
              className="mono h-[clamp(320px,calc(100dvh-380px),640px)] w-full resize-y rounded-sm border border-line bg-sunken p-3 text-caption text-fg focus:border-accent focus:outline-none"
            />
            {parseError ? (
              <p id="config-parse-error" role="alert" className="mt-1 text-caption text-error">
                {parseError}
              </p>
            ) : null}
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <Button
                icon="file"
                busy={diff.status === 'loading' || validation.status === 'loading'}
                disabled={parseError !== null}
                onClick={() => void runDiff()}
              >
                Review changes
              </Button>
              <Gate operation="applyConfig">
                <Button
                  variant="primary"
                  disabled={parseError !== null || changes.length === 0 || invalid}
                  disabledReason={
                    invalid
                      ? 'Fix the problems the review found first'
                      : changes.length === 0
                        ? 'Review your changes first; there is nothing to apply yet'
                        : undefined
                  }
                  onClick={() => setConfirm(true)}
                >
                  {changes.length > 0
                    ? `Apply ${changes.length} change${changes.length > 1 ? 's' : ''}`
                    : 'Apply changes'}
                </Button>
              </Gate>
              {applied.status === 'loading' ? null : <ApplyStatus />}
            </div>
            {applied.error ? (
              <div className="mt-3">
                <InlineError error={applied.error} />
              </div>
            ) : null}
          </>
        ) : null}
      </Section>
      <Section
        title="Changes"
        subtitle={
          diff.data
            ? `${changes.length} ${changes.length === 1 ? 'change' : 'changes'} against version ${diff.data.base_generation}`
            : 'Choose Review changes to compare your edits with the running configuration.'
        }
      >
        <ValidationResult />
        {diff.error ? <InlineError error={diff.error} /> : null}
        {diff.data && changes.length === 0 && !invalid ? (
          <EmptyState
            size="sm"
            title="No changes yet"
            description="Your edits match the running configuration."
          />
        ) : null}
        {diffWarnings.length > 0 ? (
          <ul className="mb-2 flex flex-col gap-1">
            {diffWarnings.map((w, i) => (
              <li key={i} className="text-caption text-warn">
                {w}
              </li>
            ))}
          </ul>
        ) : null}
        {diff.data && diff.data.static_ignored.length > 0 ? (
          <p className="mb-2 text-caption text-muted">
            Ignored: <span className="mono">{diff.data.static_ignored.join(', ')}</span>{' '}
            {diff.data.static_ignored.length === 1 ? 'is' : 'are'} read from the configuration file when the
            gateway starts. Change {diff.data.static_ignored.length === 1 ? 'it' : 'them'} there, then restart
            the gateway.{' '}
            <Link href={nodeHref} className="text-accent hover:underline">
              See the node&apos;s settings
            </Link>
          </p>
        ) : null}
        <ul>
          {changes.map((c, i) => (
            <ChangeRow key={`${c.path}-${i}`} change={c} />
          ))}
        </ul>
      </Section>
      <VersionsPanel />
      <ConfirmDialog
        open={confirm}
        onClose={() => setConfirm(false)}
        title="Apply changes?"
        tone="primary"
        confirmLabel="Apply changes"
        withReason
        effect={`${changes.length} ${changes.length === 1 ? 'change becomes' : 'changes become'} one new configuration version. If someone else applied a change since you loaded it, nothing is applied and you'll be asked to reload.`}
        onConfirm={async (reason) => {
          const result = await apply(reason);
          if (result)
            notify('success', translateNow('Configuration updated (version {0})', result.generation));
        }}
      />
    </div>
  );
}
