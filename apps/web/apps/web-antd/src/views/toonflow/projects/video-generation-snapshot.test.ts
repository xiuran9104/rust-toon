import { describe, expect, it } from 'vitest';

import {
  hasVideoGenerationSnapshot,
  videoGenerationRequest,
  videoReferenceManifest,
  videoStructuredShots,
} from './video-generation-snapshot';

describe('video generation snapshot', () => {
  it('reads version 2 reference and structured-shot snapshots', () => {
    const video = {
      generationContext: {
        request: {
          referenceManifest: [{ index: 1, assetId: 10, name: '沈辞' }],
          structuredShots: [{ storyboardId: 20, sequence: 1, durationSeconds: 4 }],
          version: 2,
        },
      },
    };
    expect(videoGenerationRequest(video).version).toBe(2);
    expect(videoReferenceManifest(video)[0]?.assetId).toBe(10);
    expect(videoStructuredShots(video)[0]?.storyboardId).toBe(20);
    expect(hasVideoGenerationSnapshot(video)).toBe(true);
  });

  it('keeps legacy references visible and tolerates empty contexts', () => {
    expect(videoReferenceManifest({ generation_context: { request: { references: [{ index: 1 }] } } }))
      .toHaveLength(1);
    expect(videoStructuredShots({})).toEqual([]);
    expect(hasVideoGenerationSnapshot({ generationContext: null })).toBe(false);
  });
});
