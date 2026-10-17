'use client';

import { useMemo } from 'react';

import { qrMatrix, qrPath } from '@/lib/qr';

/**
 * A QR code drawn in the browser from `value` (no network call). Always dark on white with a quiet
 * zone, whatever the theme, because that is what phone cameras read best.
 */
export function QrCode({ value, label, size = 176 }: { value: string; label: string; size?: number }) {
  const drawn = useMemo(() => {
    try {
      const matrix = qrMatrix(value);
      return { width: matrix.length + 8, path: qrPath(matrix, 4) };
    } catch {
      return null;
    }
  }, [value]);
  if (!drawn) return null;
  return (
    <svg
      role="img"
      aria-label={label}
      width={size}
      height={size}
      viewBox={`0 0 ${drawn.width} ${drawn.width}`}
      shapeRendering="crispEdges"
      className="rounded-sm border border-line"
    >
      <rect width={drawn.width} height={drawn.width} fill="#ffffff" />
      <path d={drawn.path} fill="#000000" />
    </svg>
  );
}
