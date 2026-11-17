export type AiModelType =
  | 'chat'
  | 'embedding'
  | 'image'
  | 'music'
  | 'rerank'
  | 'speech'
  | 'transcription'
  | 'video';

type ModelPreset = { label: string; model: string };
type PlatformProfile = {
  label: string;
  presets: Partial<Record<AiModelType, ModelPreset[]>>;
  types: AiModelType[];
  url: string;
};

export const AI_PLATFORM_OPTIONS: Array<{ label: string; value: string }> = [];

export const AI_MODEL_TYPE_OPTIONS = [
  { label: '对话', value: 'chat' },
  { label: '图片', value: 'image' },
  { label: '语音合成', value: 'speech' },
  { label: '视频', value: 'video' },
  { label: '向量', value: 'embedding' },
  { label: '重排', value: 'rerank' },
  { label: '语音转写', value: 'transcription' },
  { label: '音乐', value: 'music' },
] as const;

const fallbackProfile: PlatformProfile = {
  label: '',
  presets: {},
  types: [],
  url: '',
};
const gatewayProfiles = new Map<string, PlatformProfile>();
const discoveredPresets = new Map<string, ModelPreset[]>();
const modelTypeLabels = new Map(
  AI_MODEL_TYPE_OPTIONS.map((option) => [option.value, option.label]),
);

export function applyGatewayCapabilities(
  capabilities: Array<{
    label: string;
    platform: string;
    presets: Record<string, ModelPreset[]>;
    types: string[];
    url: string;
  }>,
) {
  gatewayProfiles.clear();
  AI_PLATFORM_OPTIONS.splice(
    0,
    AI_PLATFORM_OPTIONS.length,
    ...capabilities.map((item) => ({
      label: item.label,
      value: item.platform,
    })),
  );
  for (const capability of capabilities) {
    gatewayProfiles.set(capability.platform, {
      label: capability.label,
      presets: capability.presets as PlatformProfile['presets'],
      types: capability.types as AiModelType[],
      url: capability.url,
    });
  }
}

export function getAiPlatformLabel(platform?: string) {
  if (!platform) return '—';
  return gatewayProfiles.get(platform)?.label ?? platform;
}

export function getAiModelTypeLabel(type?: string) {
  if (!type) return '—';
  return modelTypeLabels.get(type as AiModelType) ?? type;
}

export function getPlatformProfile(platform?: string) {
  return (platform && gatewayProfiles.get(platform)) || fallbackProfile;
}

export function getPlatformTypeOptions(platform?: string) {
  const supported = new Set(getPlatformProfile(platform).types);
  return AI_MODEL_TYPE_OPTIONS.filter(({ value }) => supported.has(value));
}

/**
 * 选择平台后是否用平台默认地址覆盖当前地址。
 *
 * 仅当当前地址为空、或仍是上一个平台的默认地址时才覆盖：用户手动填写的
 * 本地地址（例如指向自建 OpenAI 兼容服务）必须在切换平台后保留下来，
 * 否则会静默变成平台默认地址而指向另一台并不存在的服务。
 */
export function shouldApplyPlatformDefaultUrl(
  currentUrl: string,
  previousDefaultUrl: string,
): boolean {
  const trimmed = currentUrl.trim();
  return !trimmed || trimmed === previousDefaultUrl.trim();
}

export function getModelPresetOptions(platform?: string, type?: string) {
  if (!type) return [];
  const defaults =
    getPlatformProfile(platform).presets[type as AiModelType] ?? [];
  const discovered = discoveredPresets.get(`${platform}:${type}`) ?? [];
  return [
    ...new Map(
      [...discovered, ...defaults].map((item) => [
        item.model,
        { label: item.model, model: item.model },
      ]),
    ).values(),
  ];
}

export function applyDiscoveredModels(
  platform: string,
  models: Array<{ id: string; type: string }>,
) {
  for (const type of AI_MODEL_TYPE_OPTIONS.map((item) => item.value)) {
    discoveredPresets.delete(`${platform}:${type}`);
  }
  for (const model of models) {
    const key = `${platform}:${model.type}`;
    const list = discoveredPresets.get(key) ?? [];
    list.push({ label: model.id, model: model.id });
    discoveredPresets.set(key, list);
  }
}

export function modelConfigKey(platform: string, model: string) {
  return `${platform}-${model}`
    .toLowerCase()
    .replaceAll(/[^a-z0-9]+/g, '-')
    .replaceAll(/^-|-$/g, '');
}
