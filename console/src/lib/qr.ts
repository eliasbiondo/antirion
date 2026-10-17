/**
 * A small QR code encoder (ISO/IEC 18004, byte mode, error correction level M), so the TOTP
 * enrolment can show a code for the `otpauth://` URI without a network call or a dependency. It
 * picks the smallest version that fits and the mask with the lowest penalty, as the standard asks.
 */

/** Error-correction codewords per block, level M, by version (index 0 unused). */
const ECC_PER_BLOCK = [
  -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28,
  28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
];

/** Error-correction blocks, level M, by version (index 0 unused). */
const BLOCKS = [
  -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28,
  29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
];

/** Level M's two format bits. */
const FORMAT_BITS_M = 0;

function bit(value: number, index: number): boolean {
  return ((value >>> index) & 1) !== 0;
}

function rawDataModules(version: number): number {
  let result = (16 * version + 128) * version + 64;
  if (version >= 2) {
    const align = Math.floor(version / 7) + 2;
    result -= (25 * align - 10) * align - 55;
    if (version >= 7) result -= 36;
  }
  return result;
}

function dataCodewords(version: number): number {
  return Math.floor(rawDataModules(version) / 8) - (ECC_PER_BLOCK[version] ?? 0) * (BLOCKS[version] ?? 0);
}

/** Multiplication in GF(2^8) modulo x^8 + x^4 + x^3 + x^2 + 1. */
function gfMultiply(x: number, y: number): number {
  let z = 0;
  for (let i = 7; i >= 0; i -= 1) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z;
}

function rsDivisor(degree: number): number[] {
  const result = new Array<number>(degree).fill(0);
  result[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i += 1) {
    for (let j = 0; j < degree; j += 1) {
      result[j] = gfMultiply(result[j] ?? 0, root);
      if (j + 1 < degree) result[j] = (result[j] ?? 0) ^ (result[j + 1] ?? 0);
    }
    root = gfMultiply(root, 0x02);
  }
  return result;
}

function rsRemainder(data: number[], divisor: number[]): number[] {
  const result = divisor.map(() => 0);
  for (const byte of data) {
    const factor = byte ^ (result.shift() ?? 0);
    result.push(0);
    divisor.forEach((coefficient, i) => {
      result[i] = (result[i] ?? 0) ^ gfMultiply(coefficient, factor);
    });
  }
  return result;
}

function alignmentPositions(version: number, size: number): number[] {
  if (version === 1) return [];
  const count = Math.floor(version / 7) + 2;
  const step = version === 32 ? 26 : Math.ceil((version * 4 + 4) / (count * 2 - 2)) * 2;
  const result = [6];
  for (let pos = size - 7; result.length < count; pos -= step) result.splice(1, 0, pos);
  return result;
}

/** The bytes of `text` as data codewords for `version`: mode, length, data, terminator and padding. */
function encodeData(bytes: Uint8Array, version: number): number[] {
  const bits: number[] = [];
  const push = (value: number, length: number) => {
    for (let i = length - 1; i >= 0; i -= 1) bits.push((value >>> i) & 1);
  };
  push(0b0100, 4);
  push(bytes.length, version <= 9 ? 8 : 16);
  for (const b of bytes) push(b, 8);
  const capacity = dataCodewords(version) * 8;
  push(0, Math.min(4, capacity - bits.length));
  push(0, (8 - (bits.length % 8)) % 8);
  const out: number[] = [];
  for (let i = 0; i < bits.length; i += 8) {
    let byte = 0;
    for (let j = 0; j < 8; j += 1) byte = (byte << 1) | (bits[i + j] ?? 0);
    out.push(byte);
  }
  for (let pad = 0xec; out.length < dataCodewords(version); pad ^= 0xec ^ 0x11) out.push(pad);
  return out;
}

function interleave(data: number[], version: number): number[] {
  const blocks = BLOCKS[version] ?? 1;
  const eccLength = ECC_PER_BLOCK[version] ?? 0;
  const raw = Math.floor(rawDataModules(version) / 8);
  const shortBlocks = blocks - (raw % blocks);
  const shortLength = Math.floor(raw / blocks);
  const divisor = rsDivisor(eccLength);
  const all: number[][] = [];
  for (let i = 0, k = 0; i < blocks; i += 1) {
    const block = data.slice(k, k + shortLength - eccLength + (i < shortBlocks ? 0 : 1));
    k += block.length;
    const ecc = rsRemainder(block, divisor);
    if (i < shortBlocks) block.push(0);
    all.push([...block, ...ecc]);
  }
  const result: number[] = [];
  const width = all[0]?.length ?? 0;
  for (let i = 0; i < width; i += 1) {
    all.forEach((block, j) => {
      if (i !== shortLength - eccLength || j >= shortBlocks) result.push(block[i] ?? 0);
    });
  }
  return result;
}

class Matrix {
  readonly size: number;
  readonly modules: boolean[][];
  readonly fixed: boolean[][];

  constructor(size: number) {
    this.size = size;
    this.modules = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
    this.fixed = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
  }

  set(x: number, y: number, dark: boolean): void {
    const row = this.modules[y];
    const fixedRow = this.fixed[y];
    if (!row || !fixedRow) return;
    row[x] = dark;
    fixedRow[x] = true;
  }

  at(x: number, y: number): boolean {
    return this.modules[y]?.[x] ?? false;
  }
}

function drawFunctionPatterns(m: Matrix, version: number): void {
  const { size } = m;
  for (let i = 0; i < size; i += 1) {
    m.set(6, i, i % 2 === 0);
    m.set(i, 6, i % 2 === 0);
  }
  for (const [cx, cy] of [
    [3, 3],
    [size - 4, 3],
    [3, size - 4],
  ] as const) {
    for (let dy = -4; dy <= 4; dy += 1) {
      for (let dx = -4; dx <= 4; dx += 1) {
        const distance = Math.max(Math.abs(dx), Math.abs(dy));
        const x = cx + dx;
        const y = cy + dy;
        if (x >= 0 && x < size && y >= 0 && y < size) m.set(x, y, distance !== 2 && distance !== 4);
      }
    }
  }
  const positions = alignmentPositions(version, size);
  const last = positions.length - 1;
  positions.forEach((px, i) => {
    positions.forEach((py, j) => {
      if ((i === 0 && j === 0) || (i === 0 && j === last) || (i === last && j === 0)) return;
      for (let dy = -2; dy <= 2; dy += 1)
        for (let dx = -2; dx <= 2; dx += 1)
          m.set(px + dx, py + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
    });
  });
  drawFormat(m, 0);
  if (version >= 7) {
    let rem = version;
    for (let i = 0; i < 12; i += 1) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
    const bits = (version << 12) | rem;
    for (let i = 0; i < 18; i += 1) {
      const a = size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      m.set(a, b, bit(bits, i));
      m.set(b, a, bit(bits, i));
    }
  }
}

function drawFormat(m: Matrix, mask: number): void {
  const data = (FORMAT_BITS_M << 3) | mask;
  let rem = data;
  for (let i = 0; i < 10; i += 1) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  const bits = ((data << 10) | rem) ^ 0x5412;
  const { size } = m;
  for (let i = 0; i <= 5; i += 1) m.set(8, i, bit(bits, i));
  m.set(8, 7, bit(bits, 6));
  m.set(8, 8, bit(bits, 7));
  m.set(7, 8, bit(bits, 8));
  for (let i = 9; i < 15; i += 1) m.set(14 - i, 8, bit(bits, i));
  for (let i = 0; i < 8; i += 1) m.set(size - 1 - i, 8, bit(bits, i));
  for (let i = 8; i < 15; i += 1) m.set(8, size - 15 + i, bit(bits, i));
  m.set(8, size - 8, true);
}

function drawCodewords(m: Matrix, codewords: number[]): void {
  const { size } = m;
  let i = 0;
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vert = 0; vert < size; vert += 1) {
      for (let j = 0; j < 2; j += 1) {
        const x = right - j;
        const upward = ((right + 1) & 2) === 0;
        const y = upward ? size - 1 - vert : vert;
        if (!m.fixed[y]?.[x] && i < codewords.length * 8) {
          const row = m.modules[y];
          if (row) row[x] = bit(codewords[i >>> 3] ?? 0, 7 - (i & 7));
          i += 1;
        }
      }
    }
  }
}

function maskBit(mask: number, x: number, y: number): boolean {
  switch (mask) {
    case 0:
      return (x + y) % 2 === 0;
    case 1:
      return y % 2 === 0;
    case 2:
      return x % 3 === 0;
    case 3:
      return (x + y) % 3 === 0;
    case 4:
      return (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0;
    case 5:
      return ((x * y) % 2) + ((x * y) % 3) === 0;
    case 6:
      return (((x * y) % 2) + ((x * y) % 3)) % 2 === 0;
    default:
      return (((x + y) % 2) + ((x * y) % 3)) % 2 === 0;
  }
}

function applyMask(m: Matrix, mask: number): void {
  for (let y = 0; y < m.size; y += 1) {
    const row = m.modules[y];
    for (let x = 0; x < m.size; x += 1) {
      if (row && !m.fixed[y]?.[x] && maskBit(mask, x, y)) row[x] = !row[x];
    }
  }
}

/** The standard's penalty score: runs, 2 × 2 blocks, finder-like patterns and dark balance. */
function penalty(m: Matrix): number {
  const { size } = m;
  let score = 0;
  const line = (get: (i: number) => boolean) => {
    let runColor = false;
    let run = 0;
    const history = [0, 0, 0, 0, 0, 0, 0];
    const add = (length: number) => {
      history.pop();
      history.unshift(history[0] === 0 ? length + size : length);
    };
    const finder = () => {
      const n = history[1] ?? 0;
      const core = n > 0 && history[2] === n && history[3] === n * 3 && history[4] === n && history[5] === n;
      return (
        (core && (history[0] ?? 0) >= n * 4 && (history[6] ?? 0) >= n ? 1 : 0) +
        (core && (history[6] ?? 0) >= n * 4 && (history[0] ?? 0) >= n ? 1 : 0)
      );
    };
    for (let i = 0; i < size; i += 1) {
      const dark = get(i);
      if (dark === runColor) {
        run += 1;
        if (run === 5) score += 3;
        else if (run > 5) score += 1;
      } else {
        add(run);
        if (!runColor) score += finder() * 40;
        runColor = dark;
        run = 1;
      }
    }
    if (runColor) {
      add(run);
      run = 0;
    }
    add(run + size);
    score += finder() * 40;
  };
  for (let y = 0; y < size; y += 1) line((x) => m.at(x, y));
  for (let x = 0; x < size; x += 1) line((y) => m.at(x, y));
  let dark = 0;
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      const c = m.at(x, y);
      if (c) dark += 1;
      if (
        x < size - 1 &&
        y < size - 1 &&
        c === m.at(x + 1, y) &&
        c === m.at(x, y + 1) &&
        c === m.at(x + 1, y + 1)
      )
        score += 3;
    }
  }
  const total = size * size;
  score += (Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1) * 10;
  return score;
}

/** The module matrix (`true` is dark) of a QR code holding `text` in UTF-8. */
export function qrMatrix(text: string): boolean[][] {
  const bytes = new TextEncoder().encode(text);
  let version = 1;
  while (version <= 40 && 4 + (version <= 9 ? 8 : 16) + bytes.length * 8 > dataCodewords(version) * 8)
    version += 1;
  if (version > 40) throw new Error('Text too long for a QR code');
  const size = version * 4 + 17;
  const m = new Matrix(size);
  drawFunctionPatterns(m, version);
  drawCodewords(m, interleave(encodeData(bytes, version), version));
  let best = 0;
  let bestScore = Infinity;
  for (let mask = 0; mask < 8; mask += 1) {
    applyMask(m, mask);
    drawFormat(m, mask);
    const score = penalty(m);
    if (score < bestScore) {
      best = mask;
      bestScore = score;
    }
    applyMask(m, mask);
  }
  applyMask(m, best);
  drawFormat(m, best);
  return m.modules;
}

/** An SVG path (one unit per module, offset by `margin`) drawing the dark modules of `matrix`. */
export function qrPath(matrix: boolean[][], margin = 4): string {
  const parts: string[] = [];
  matrix.forEach((row, y) => {
    row.forEach((dark, x) => {
      if (dark) parts.push(`M${x + margin} ${y + margin}h1v1h-1z`);
    });
  });
  return parts.join('');
}
