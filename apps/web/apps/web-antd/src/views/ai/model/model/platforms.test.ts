import { describe, expect, it } from 'vitest';

import { shouldApplyPlatformDefaultUrl } from './platforms';

describe('platform default API address', () => {
  it('applies the platform default when the address is empty', () => {
    expect(shouldApplyPlatformDefaultUrl('', 'http://127.0.0.1:11434/v1')).toBe(
      true,
    );
    expect(shouldApplyPlatformDefaultUrl('   ', 'http://127.0.0.1:11434/v1')).toBe(
      true,
    );
  });

  it('applies the platform default when the address is still the previous default', () => {
    expect(
      shouldApplyPlatformDefaultUrl(
        'http://127.0.0.1:11434/v1',
        'http://127.0.0.1:11434/v1',
      ),
    ).toBe(true);
  });

  it('keeps a manually entered local address when the platform changes', () => {
    expect(
      shouldApplyPlatformDefaultUrl(
        'http://127.0.0.1:8000',
        'http://127.0.0.1:11434/v1',
      ),
    ).toBe(false);
    expect(
      shouldApplyPlatformDefaultUrl(
        'http://127.0.0.1:8000/v1',
        'http://127.0.0.1:11434/v1',
      ),
    ).toBe(false);
  });
});
