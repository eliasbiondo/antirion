'use client';

import Link from 'next/link';
import { useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { Gate, useCan } from '@/components/ui/gate';
import { Section } from '@/components/ui/layout';
import { MenuButton } from '@/components/ui/menu';
import { EmptyState } from '@/components/ui/states';
import { ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import { formatCount } from '@/lib/format';

import { useCatalog } from '../store';
import type { ModelView } from '../types';
import { AliasDialog, RemoveAliasDialog } from './alias-dialog';

interface AliasRow {
  alias: string;
  model: string;
  description: string;
  owner: string;
  requests: number | null;
}

/** Every alias across the catalog, with its Model, owner and 30-day requests; added, edited and removed here. */
export function AliasesTab({
  views,
  onOpenModel,
}: {
  views: ModelView[];
  onOpenModel: (name: string) => void;
}) {
  const routingHref = useScopedHref('/routing/');
  const requested = useCatalog((s) => s.requested.data);
  const canEdit = useCan('replaceModel');
  const [dialog, setDialog] = useState<
    | { kind: 'edit'; model: string | null; alias: string | null }
    | { kind: 'remove'; model: string; alias: string }
    | null
  >(null);
  const rows: AliasRow[] = views.flatMap((v) =>
    Object.entries(v.model.spec.aliases ?? {}).map(([alias, a]) => ({
      alias,
      model: v.name,
      description: a.description ?? '',
      owner: a.owner ?? '',
      requests: requested ? (requested.get(alias) ?? 0) : null,
    })),
  );
  return (
    <>
      <Section
        flush
        title="Aliases"
        subtitle={
          <>
            Another name apps can send for a model, with its own settings. Rules by team, tag or size are on{' '}
            <Link href={routingHref} className="text-accent hover:underline">
              Routing policy
            </Link>
            .
          </>
        }
        actions={
          <>
            {rows.length > 0 ? (
              <ExportCsvButton
                filename="aliases.csv"
                rows={rows}
                columns={[
                  { header: 'alias', value: (r) => r.alias },
                  { header: 'model', value: (r) => r.model },
                  { header: 'owner', value: (r) => r.owner },
                  { header: 'description', value: (r) => r.description },
                  { header: 'requests_30d', value: (r) => r.requests },
                ]}
              />
            ) : null}
            <Gate operation="replaceModel" mode="hide">
              <Button
                size="sm"
                icon="plus"
                onClick={() => setDialog({ kind: 'edit', model: null, alias: null })}
              >
                Add alias
              </Button>
            </Gate>
          </>
        }
      >
        {rows.length === 0 ? (
          <EmptyState
            size="sm"
            title="No aliases"
            description="Give a model a second name, such as chat-default, that apps can send."
          />
        ) : (
          <div className="overflow-x-auto">
            <Table label="Aliases">
              <thead>
                <tr>
                  <Th>Alias</Th>
                  <Th>Model</Th>
                  <Th>Owner</Th>
                  <Th>Description</Th>
                  <Th numeric>Requests, 30 days</Th>
                  {canEdit ? (
                    <Th className="w-12">
                      <span className="sr-only">Actions</span>
                    </Th>
                  ) : null}
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <Tr key={`${r.model}/${r.alias}`}>
                    <Td mono className="text-accent">
                      @{r.alias}
                    </Td>
                    <Td mono>
                      <button
                        type="button"
                        className="hover:text-accent hover:underline"
                        onClick={() => onOpenModel(r.model)}
                      >
                        {r.model}
                      </button>
                    </Td>
                    <Td muted>{r.owner || '—'}</Td>
                    <Td muted>{r.description || '—'}</Td>
                    <Td numeric>{r.requests === null ? '—' : formatCount(r.requests)}</Td>
                    {canEdit ? (
                      <Td>
                        <MenuButton
                          iconOnly
                          icon="more"
                          variant="ghost"
                          size="sm"
                          align="right"
                          label={`Actions for @${r.alias}`}
                          items={[
                            {
                              label: 'Edit',
                              icon: 'edit',
                              onSelect: () => setDialog({ kind: 'edit', model: r.model, alias: r.alias }),
                            },
                            {
                              label: 'Remove',
                              icon: 'trash',
                              danger: true,
                              onSelect: () => setDialog({ kind: 'remove', model: r.model, alias: r.alias }),
                            },
                          ]}
                        />
                      </Td>
                    ) : null}
                  </Tr>
                ))}
              </tbody>
            </Table>
          </div>
        )}
      </Section>
      {dialog?.kind === 'edit' ? (
        <AliasDialog model={dialog.model} alias={dialog.alias} onClose={() => setDialog(null)} />
      ) : null}
      {dialog?.kind === 'remove' ? (
        <RemoveAliasDialog model={dialog.model} alias={dialog.alias} onClose={() => setDialog(null)} />
      ) : null}
    </>
  );
}
