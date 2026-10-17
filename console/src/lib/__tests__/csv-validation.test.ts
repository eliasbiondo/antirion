import { describe, expect, it } from 'vitest';

import { ApiError } from '../api/client';
import { toCsv } from '../csv';
import { cidr, money, resourceName, serverFieldErrors, splitList, validate } from '../validation';

describe('csv', () => {
  it('writes RFC 4180 with quoting and CRLF', () => {
    expect(
      toCsv(
        ['a', 'b'],
        [
          ['x,y', 'say "hi"'],
          [1, null],
        ],
      ),
    ).toBe('a,b\r\n"x,y","say ""hi"""\r\n1,\r\n');
  });
});

describe('validation', () => {
  it('checks resource names, money and CIDRs', () => {
    expect(resourceName.safeParse('ci-bot').success).toBe(true);
    expect(resourceName.safeParse('CI Bot').success).toBe(false);
    expect(money.safeParse('2.50').success).toBe(true);
    expect(money.safeParse('-1').success).toBe(false);
    expect(cidr.safeParse('10.0.0.0/8').success).toBe(true);
    expect(cidr.safeParse('@office').success).toBe(true);
    expect(cidr.safeParse('not an ip').success).toBe(false);
  });
  it('keys errors by field path', () => {
    const r = validate(resourceName, 'Bad Name');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.errors['']).toMatch(/Lowercase/);
  });
  it('maps server validation details onto fields', () => {
    const e = new ApiError({
      status: 400,
      code: 'validation_failed',
      message: '1 error',
      requestId: 'r',
      operation: 'POST /keys',
      details: [{ path: 'keys.ci.allowed_ips[0]', code: 'invalid_format', message: 'not a CIDR' }],
    });
    expect(serverFieldErrors(e, 'keys.ci')).toEqual({ allowed_ips: 'not a CIDR' });
  });
  it('splits lists', () => {
    expect(splitList('a, b\nc,,')).toEqual(['a', 'b', 'c']);
  });
});
