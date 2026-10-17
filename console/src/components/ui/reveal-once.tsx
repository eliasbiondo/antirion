'use client';

import { useState, type ReactNode } from 'react';

import { Button } from './button';
import { Checkbox } from './choice';
import { Icon } from './icon';
import { Dialog } from './overlay';

export interface RevealOnceDialogProps {
  /** The value the API returned once; null closes the dialog. Held only in the caller's memory. */
  secret: string | null;
  onClose: () => void;
  title: string;
  description?: ReactNode;
  label?: string;
  /** Extra help under the secret, such as how to use it ("Use it" snippets). */
  children?: ReactNode;
}

/**
 * The only place a secret is ever on screen: a value the API returned once. It offers copy, warns
 * that it will not be shown again, and the caller drops the value when this closes.
 */
export function RevealOnceDialog({
  secret,
  onClose,
  title,
  description,
  label = 'Secret',
  children,
}: RevealOnceDialogProps) {
  const [copied, setCopied] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);

  function close() {
    setCopied(false);
    setAcknowledged(false);
    onClose();
  }

  async function copy() {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <Dialog
      open={secret !== null}
      onClose={close}
      title={title}
      description={description}
      size="md"
      role="alertdialog"
      footer={
        <Button
          variant="primary"
          onClick={close}
          disabled={!acknowledged && !copied}
          disabledReason="Copy it or tick “I’ve saved it” to continue"
        >
          Done
        </Button>
      }
    >
      <div className="flex flex-col gap-4">
        <div className="flex items-start gap-2 rounded-sm border border-[color-mix(in_oklab,var(--ant-warn)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-warn)_8%,transparent)] px-3 py-2.5 text-ui text-fg">
          <span className="mt-0.5 text-warn">
            <Icon name="alert" size={14} />
          </span>
          <span>
            Copy this now. You won&apos;t be able to see it again; if you lose it, you&apos;ll need to create
            a new one.
          </span>
        </div>
        <div>
          <div className="eyebrow mb-1.5">{label}</div>
          <div className="flex items-stretch gap-2">
            <output
              aria-label={label}
              className="mono flex-1 rounded-sm border border-line bg-sunken px-3 py-2 text-body break-all text-fg select-all"
              data-secret
            >
              {secret}
            </output>
            <Button icon={copied ? 'check' : 'copy'} onClick={() => void copy()} data-autofocus>
              {copied ? 'Copied' : 'Copy'}
            </Button>
          </div>
        </div>
        {children}
        <Checkbox
          checked={acknowledged}
          onChange={setAcknowledged}
          label={<>I&apos;ve saved it somewhere safe</>}
        />
      </div>
    </Dialog>
  );
}
