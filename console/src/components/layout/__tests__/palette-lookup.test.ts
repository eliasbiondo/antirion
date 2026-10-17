import { describe, expect, it } from 'vitest';

import { looksLikeRequestId } from '../command-palette';

describe('the palette offers to open a request only for an id', () => {
  it('takes a record or request id and leaves other text to the search', () => {
    expect(looksLikeRequestId('01a144e7-62e8-7049-9f29-296d6857d9a1')).toBe(true);
    expect(looksLikeRequestId('01A144E7-62E8-7049-9F29-296D6857D9A1')).toBe(true);
    expect(looksLikeRequestId('zendesk-assistant')).toBe(false);
    expect(looksLikeRequestId('error-rate-check')).toBe(false);
    expect(looksLikeRequestId('01a144e7-62e8')).toBe(false);
  });
});
