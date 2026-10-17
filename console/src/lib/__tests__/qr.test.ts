import { describe, expect, it } from 'vitest';

import { qrMatrix, qrPath } from '../qr';

/** Reads the 15 format bits around the top-left finder, undoes the mask and checks the BCH code. */
function formatOf(m: boolean[][]): { level: number; mask: number } {
  const at = (x: number, y: number) => (m[y]?.[x] ? 1 : 0);
  let bits = 0;
  const order: [number, number][] = [
    [8, 0],
    [8, 1],
    [8, 2],
    [8, 3],
    [8, 4],
    [8, 5],
    [8, 7],
    [8, 8],
    [7, 8],
    [5, 8],
    [4, 8],
    [3, 8],
    [2, 8],
    [1, 8],
    [0, 8],
  ];
  order.forEach(([x, y], i) => {
    bits |= at(x, y) << i;
  });
  const raw = bits ^ 0x5412;
  const data = raw >>> 10;
  let rem = data;
  for (let i = 0; i < 10; i += 1) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  expect(((data << 10) | rem) & 0x7fff).toBe(raw);
  return { level: data >>> 3, mask: data & 7 };
}

function finderAt(m: boolean[][], x0: number, y0: number): boolean {
  for (let dy = 0; dy < 7; dy += 1)
    for (let dx = 0; dx < 7; dx += 1) {
      const ring = Math.max(Math.abs(dx - 3), Math.abs(dy - 3));
      if (Boolean(m[y0 + dy]?.[x0 + dx]) !== (ring !== 2)) return false;
    }
  return true;
}

describe('QR encoder', () => {
  it('picks the smallest version that fits and draws the three finder patterns', () => {
    expect(qrMatrix('hi')).toHaveLength(21);
    const uri =
      'otpauth://totp/Antirion:ada@example.com?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=Antirion';
    const m = qrMatrix(uri);
    expect(m).toHaveLength(41);
    const n = m.length;
    expect(finderAt(m, 0, 0)).toBe(true);
    expect(finderAt(m, n - 7, 0)).toBe(true);
    expect(finderAt(m, 0, n - 7)).toBe(true);
    // Timing pattern and the always-dark module.
    for (let i = 8; i < n - 8; i += 1) expect(m[6]?.[i]).toBe(i % 2 === 0);
    expect(m[n - 8]?.[8]).toBe(true);
  });

  it('writes valid format information for level M', () => {
    for (const text of ['a', 'otpauth://totp/x?secret=ABC', 'x'.repeat(300)]) {
      const { level, mask } = formatOf(qrMatrix(text));
      expect(level).toBe(0);
      expect(mask).toBeGreaterThanOrEqual(0);
      expect(mask).toBeLessThan(8);
    }
  });

  it('draws one unit square per dark module, offset by the quiet zone', () => {
    const m = qrMatrix('hi');
    const dark = m.flat().filter(Boolean).length;
    const path = qrPath(m, 4);
    expect(path.match(/M/g)).toHaveLength(dark);
    expect(path.startsWith('M4 4h1v1h-1z')).toBe(true);
  });
});
