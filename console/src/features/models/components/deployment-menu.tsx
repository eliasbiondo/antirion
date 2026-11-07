'use client';

import { useCan } from '@/components/ui/gate';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { describeError, isChangeControl } from '@/lib/api/client';
import type { DeploymentHealth, DeploymentResource } from '@/lib/api/types';
import { notify } from '@/stores/ui';

import { runtimeStatus, useDeploymentDisables } from '../deployment-state';
import { setDeploymentEnabled, useCatalog } from '../store';
import { resumeNow, turnOn } from './deployment-runtime';
import { translateNow } from '@/lib/i18n';

/** Re-reads the deployments' health and who turned them off, after a runtime action. */
export async function reloadHealth(): Promise<void> {
  await Promise.all([useCatalog.getState().loadHealth(), useDeploymentDisables.getState().load()]);
}

/**
 * A deployment row's actions: edit it; turn it off by hand or back on; resume it after a pause.
 * "Turn on" clears what keeps it off: a manual disable (`:enable`) or `enabled: false` in its settings.
 */
export function DeploymentMenu({
  deployment,
  health,
  onEdit,
  onTurnOff,
}: {
  deployment: DeploymentResource;
  health: DeploymentHealth | undefined;
  onEdit: () => void;
  onTurnOff: () => void;
}) {
  const canEdit = useCan('replaceDeployment');
  const canPatch = useCan('patchDeployment');
  const canEnable = useCan('enableDeployment');
  const canDisable = useCan('disableDeployment');
  const canReset = useCan('resetDeployment');
  const name = deployment.name;
  const status = runtimeStatus(health);
  const offInSettings = deployment.spec.enabled === false;
  const items: MenuItem[] = [
    ...(canEdit ? [{ label: 'Edit deployment', icon: 'edit' as const, onSelect: onEdit }] : []),
    ...(status.action === 'enable' && canEnable
      ? [{ label: 'Turn on', icon: 'play' as const, onSelect: () => void turnOn(name, reloadHealth) }]
      : []),
    ...(status.action !== 'enable' && offInSettings && canPatch
      ? [
          {
            label: 'Turn on',
            icon: 'play' as const,
            onSelect: () =>
              void setDeploymentEnabled(name, true).then(
                () => notify('success', 'Deployment turned on', name),
                (e: unknown) => {
                  if (!isChangeControl(e))
                    notify('error', translateNow('Couldn’t turn on {0}', name), describeError(e));
                },
              ),
          },
        ]
      : []),
    ...(status.action === 'reset' && canReset
      ? [
          {
            label: 'Resume now',
            icon: 'refresh' as const,
            onSelect: () => void resumeNow(name, reloadHealth),
          },
        ]
      : []),
    ...(status.kind !== 'off' && !offInSettings && canDisable
      ? [{ label: 'Turn off…', icon: 'pause' as const, danger: true, onSelect: onTurnOff }]
      : []),
  ];
  if (items.length === 0) return null;
  return (
    <MenuButton
      iconOnly
      icon="more"
      variant="ghost"
      size="sm"
      align="right"
      label={`Actions for ${name}`}
      items={items}
    />
  );
}
