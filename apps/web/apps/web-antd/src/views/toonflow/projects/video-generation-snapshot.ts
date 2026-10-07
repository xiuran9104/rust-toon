export interface VideoReferenceSnapshot {
  assetId?: number;
  assetType?: string;
  imageId?: number;
  index: number;
  kind?: string;
  name?: string;
  role?: string;
  url?: string;
}

export interface DroppedReferenceSnapshot {
  assetId?: number;
  assetType?: string;
  filePath?: string;
  imageId?: number;
  name?: string;
  reason?: string;
}

export interface StructuredShotSnapshot {
  description?: string;
  durationSeconds?: number;
  references?: Array<{
    assetId?: number;
    assetType?: string;
    index?: number;
    name?: string;
  }>;
  sceneKey?: string;
  sceneStateKey?: string;
  sequence?: number;
  storyboardId?: number;
}

function objectValue(value: unknown): Record<string, any> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, any>)
    : {};
}

export function videoGenerationRequest(video: any) {
  const context = objectValue(video?.generationContext ?? video?.generation_context);
  return objectValue(context.request);
}

export function videoReferenceManifest(video: any): VideoReferenceSnapshot[] {
  const request = videoGenerationRequest(video);
  const value = request.referenceManifest ?? request.references;
  return Array.isArray(value) ? value : [];
}

export function videoDroppedReferences(video: any): DroppedReferenceSnapshot[] {
  const value = videoGenerationRequest(video).droppedReferences;
  return Array.isArray(value) ? value : [];
}

export function videoStructuredShots(video: any): StructuredShotSnapshot[] {
  const value = videoGenerationRequest(video).structuredShots;
  return Array.isArray(value) ? value : [];
}

export function hasVideoGenerationSnapshot(video: any) {
  const context = objectValue(video?.generationContext ?? video?.generation_context);
  return Object.keys(context).length > 0;
}
