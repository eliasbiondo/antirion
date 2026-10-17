'use client';

import { useCallback, useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate } from '@/components/ui/gate';
import { Section } from '@/components/ui/layout';
import { Drawer } from '@/components/ui/overlay';
import { ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { downloadText } from '@/lib/csv';
import { asApiError, emptySlot, type Slot } from '@/lib/slot';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { type ConfigDoc, type GenerationSummary, settingsApi } from '../api';
import { compareDocuments } from '../doc-diff';
import { useConfigDoc } from '../store';
import { ChangeRow } from './change-row';
import { auditActorLabel } from '@/lib/audit-words';

/** Where a version came from, in words. */
export function sourceLabel(source: string): string {
  if (source === 'api') return 'Console or API';
  if (source === 'rollback') return 'Rollback';
  if (source === 'apply:file') return 'Configuration file';
  if (source.startsWith('apply:')) return `Applied from ${source.slice('apply:'.length)}`;
  return source;
}

function actorLabel(actor: GenerationSummary['actor']): string {
  if (actor.type === 'system') return 'The gateway';
  return auditActorLabel({ type: actor.type ?? null, id: actor.id ?? null });
}

interface Opened {
  version: GenerationSummary;
  doc: Slot<ConfigDoc>;
}

function VersionDrawer({
  opened,
  running,
  onClose,
  onRollback,
}: {
  opened: Opened | null;
  running: number | null;
  onClose: () => void;
  onRollback: () => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const v = opened?.version;
  const doc = opened?.doc.data ?? null;
  const current = useConfigDoc((st) => st.document.data?.doc ?? null);
  const changes = doc && current ? compareDocuments(current, doc) : null;
  const isRunning = v?.generation === running;
  return (
    <Drawer
      open={opened !== null}
      onClose={onClose}
      width="lg"
      title={v ? `Version ${v.generation}` : ''}
      subtitle={
        v
          ? `${formatDateTime(v.created_at, timeZone)} · ${actorLabel(v.actor)} · ${sourceLabel(v.source)}`
          : undefined
      }
      footer={
        v ? (
          <>
            <Button
              variant="ghost"
              icon="download"
              disabled={!doc}
              onClick={() =>
                downloadText(
                  `antirion-config-v${v.generation}.json`,
                  JSON.stringify(doc ?? {}, null, 2),
                  'application/json',
                )
              }
            >
              Download
            </Button>
            <Gate operation="rollbackConfig">
              <Button
                variant="primary"
                icon="history"
                disabled={isRunning || !changes || changes.length === 0}
                disabledReason={isRunning ? 'This version is running now' : 'Nothing would change'}
                onClick={onRollback}
              >
                Roll back to this version
              </Button>
            </Gate>
          </>
        ) : null
      }
    >
      {opened ? (
        <div className="flex flex-col gap-4">
          <p className="text-ui text-muted">{v?.summary}</p>
          {opened.doc.error ? <ErrorState error={opened.doc.error} subject="This version" size="sm" /> : null}
          {opened.doc.status === 'loading' ? <LoadingRows rows={4} /> : null}
          {changes ? (
            <div>
              <h3 className="text-ui font-semibold text-fg">
                {isRunning
                  ? 'This version is running now'
                  : changes.length === 0
                    ? 'Same as the running configuration'
                    : `Rolling back would change ${changes.length} ${changes.length === 1 ? 'setting' : 'settings'}`}
              </h3>
              {!isRunning && changes.length > 0 ? (
                <p className="mt-0.5 text-caption text-muted">
                  Left: the running version {running}. Right: version {v?.generation}.
                </p>
              ) : null}
              <ul className="mt-2">
                {changes.map((c, i) => (
                  <ChangeRow key={`${c.path}-${i}`} change={c} />
                ))}
              </ul>
            </div>
          ) : null}
          {opened.doc.data ? (
            <details className="group">
              <summary className="w-fit cursor-pointer text-caption text-muted hover:text-fg">
                The whole document
              </summary>
              <pre className="mono mt-2 max-h-[50vh] overflow-auto rounded-sm border border-line bg-sunken p-3 text-caption text-fg">
                {JSON.stringify(opened.doc.data, null, 2)}
              </pre>
            </details>
          ) : null}
        </div>
      ) : null}
    </Drawer>
  );
}

/** Configuration versions: who changed it, when and how, with a drawer to compare and roll back. */
export function VersionsPanel() {
  const timeZone = usePreferences((s) => s.timeZone);
  const document = useConfigDoc((s) => s.document);
  const loadDocument = useConfigDoc((s) => s.loadDocument);
  const [versions, setVersions] = useState<Slot<GenerationSummary[]>>(emptySlot());
  const [opened, setOpened] = useState<Opened | null>(null);
  const [confirm, setConfirm] = useState(false);
  const running = document.data?.etag ? Number(document.data.etag.replace(/"/g, '')) : null;

  const load = useCallback(async () => {
    setVersions((v) => ({ ...v, status: v.data ? v.status : 'loading', error: null }));
    try {
      const data = await settingsApi.generations();
      setVersions({ data, status: 'ready', error: null, at: Date.now() });
    } catch (e) {
      setVersions((v) => ({ ...v, status: 'error', error: asApiError(e) }));
    }
  }, []);

  useEffect(() => {
    if (running !== null) void load();
  }, [running, load]);

  function open(version: GenerationSummary) {
    setOpened({ version, doc: { ...emptySlot(), status: 'loading' } });
    settingsApi.generation(version.generation).then(
      (doc) =>
        setOpened((o) =>
          o?.version.generation === version.generation
            ? { ...o, doc: { data: doc, status: 'ready', error: null, at: Date.now() } }
            : o,
        ),
      (e: unknown) =>
        setOpened((o) =>
          o?.version.generation === version.generation
            ? { ...o, doc: { data: null, status: 'error', error: asApiError(e), at: null } }
            : o,
        ),
    );
  }

  const list = versions.data ?? [];
  const target = opened?.version.generation;
  const current = document.data?.doc ?? null;
  const changeCount = opened?.doc.data && current ? compareDocuments(current, opened.doc.data).length : 0;
  return (
    <Section
      title="Versions"
      subtitle="Every change makes a new version. Open one to see what rolling back to it would change."
    >
      {versions.status === 'loading' && !versions.data ? <LoadingRows rows={4} /> : null}
      {versions.error ? (
        <ErrorState
          error={versions.error}
          subject="Configuration versions"
          size="sm"
          onRetry={() => void load()}
        />
      ) : null}
      {list.length > 0 ? (
        <div className="max-h-[420px] overflow-y-auto">
          <Table label="Configuration versions">
            <thead>
              <tr>
                <Th numeric>Version</Th>
                <Th>When</Th>
                <Th>Who</Th>
                <Th className="max-md:hidden">How</Th>
                <Th className="max-md:hidden">What changed</Th>
              </tr>
            </thead>
            <tbody>
              {list.map((g) => (
                <Tr
                  key={g.generation}
                  interactive
                  tabIndex={0}
                  onClick={() => open(g)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') open(g);
                  }}
                >
                  <Td numeric>
                    <span className="inline-flex items-center gap-2">
                      {g.generation === running ? <Badge tone="success">Running</Badge> : null}
                      {g.generation}
                    </span>
                  </Td>
                  <Td muted title={formatDateTime(g.created_at, timeZone)}>
                    {formatRelative(g.created_at)}
                  </Td>
                  <Td>
                    <span
                      className={g.actor.type === 'system' || g.actor.id === 'bootstrap' ? undefined : 'mono'}
                    >
                      {actorLabel(g.actor)}
                    </span>
                  </Td>
                  <Td muted className="max-md:hidden">
                    {sourceLabel(g.source)}
                  </Td>
                  <Td muted className="max-md:hidden">
                    {g.summary}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        </div>
      ) : null}
      <VersionDrawer
        opened={opened}
        running={running}
        onClose={() => setOpened(null)}
        onRollback={() => setConfirm(true)}
      />
      <ConfirmDialog
        open={confirm}
        onClose={() => setConfirm(false)}
        title={`Roll back to version ${target ?? ''}?`}
        tone="primary"
        confirmLabel="Roll back"
        withReason
        effect={`A new version is made with the configuration of version ${target ?? ''}: ${changeCount} ${changeCount === 1 ? 'setting changes' : 'settings change'}. Anything created since then, keys included, is removed. If someone changes the configuration meanwhile, nothing is applied.`}
        onConfirm={async (reason) => {
          if (target === undefined) return;
          const result = await settingsApi.rollback(target, document.data?.etag ?? null, reason);
          notify(
            'success',
            `Rolled back to version ${target}`,
            `Now running as version ${result.generation}.`,
          );
          setOpened(null);
          await loadDocument();
        }}
      />
    </Section>
  );
}
