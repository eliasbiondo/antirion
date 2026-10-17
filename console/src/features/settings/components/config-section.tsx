'use client';

import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate } from '@/components/ui/gate';
import { Section } from '@/components/ui/layout';
import { EmptyState, ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { downloadText } from '@/lib/csv';
import { notify } from '@/stores/ui';

import { useConfigDoc } from '../store';
import { useReportUnsaved } from '../unsaved';
import { ChangeRow } from './change-row';
import { VersionsPanel } from './versions-panel';

/** The configuration document as JSON: edit it, see the diff against the running generation, apply. */
export function ConfigSection() {
  const document = useConfigDoc((s) => s.document);
  const draft = useConfigDoc((s) => s.draft);
  const parseError = useConfigDoc((s) => s.parseError);
  const diff = useConfigDoc((s) => s.diff);
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

  const changes = diff.data?.changes ?? [];
  return (
    <div className="flex flex-col">
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
        {document.error ? (
          <ErrorState
            error={document.error}
            subject="The configuration"
            onRetry={() => void loadDocument()}
          />
        ) : null}
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
            <div className="mt-3 flex items-center gap-2">
              <Button
                icon="file"
                busy={diff.status === 'loading'}
                disabled={parseError !== null}
                onClick={() => void runDiff()}
              >
                Review changes
              </Button>
              <Gate operation="applyConfig">
                <Button
                  variant="primary"
                  disabled={parseError !== null || changes.length === 0}
                  disabledReason={
                    changes.length === 0
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
              {applied.data ? (
                <span className="text-caption text-success">
                  Applied as version {applied.data.generation}
                  {applied.data.propagated ? ', live on every node' : ', reaching every node'}
                </span>
              ) : null}
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
        {diff.error ? <InlineError error={diff.error} /> : null}
        {diff.data && changes.length === 0 ? (
          <EmptyState
            size="sm"
            title="No changes yet"
            description="Your edits match the running configuration."
          />
        ) : null}
        {(diff.data?.warnings ?? []).length > 0 ? (
          <ul className="mb-2 flex flex-col gap-1">
            {(diff.data?.warnings ?? []).map((w, i) => (
              <li key={i} className="text-caption text-warn">
                {typeof w === 'string' ? w : ((w as { message?: string }).message ?? JSON.stringify(w))}
              </li>
            ))}
          </ul>
        ) : null}
        {diff.data && diff.data.static_ignored.length > 0 ? (
          <p className="mb-2 text-caption text-muted">
            Not changed until the gateway restarts:{' '}
            <span className="mono">{diff.data.static_ignored.join(', ')}</span>
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
          if (result) notify('success', `Configuration updated (version ${result.generation})`);
        }}
      />
    </div>
  );
}
