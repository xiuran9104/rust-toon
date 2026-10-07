<script lang="ts" setup>
import { computed, nextTick, onActivated, onMounted, ref, watch } from 'vue';

import { AiModelTypeEnum } from '@vben/constants';

import {
  Alert,
  Button,
  Checkbox,
  Empty,
  Input,
  InputNumber,
  Modal,
  Select,
  Slider,
  Switch,
  Tag,
  Tooltip,
} from 'ant-design-vue';

import { getModelSimpleList } from '#/api/ai/model/model';
import { inspectTrackVideo, updateVideoTransitionSettings } from '#/api/toonflow';
import { videoQualityPresentation } from './video-quality';
import { assetFileUrl } from '../assets/asset-types';
import StoryboardQuickPreview from './StoryboardQuickPreview.vue';
import StoryboardTrackStrip from './StoryboardTrackStrip.vue';
import { storyboardsForTrack } from './storyboard-track-groups';
import { defaultVideoGenerationMode, videoFrameRole } from './video-generation-mode';
import { groupVideoTracksByScene } from './video-scene-groups';
import {
  hasVideoGenerationSnapshot,
  videoDroppedReferences,
  videoGenerationRequest,
  videoReferenceManifest,
  videoStructuredShots,
} from './video-generation-snapshot';
import {
  normalizeVideoTransitionSettings,
  previousVideoTrackContext,
  transitionDurationApplies,
  transitionSourceLabel,
  videoFrameApplication,
  VIDEO_FRAME_POLICY_OPTIONS,
  VIDEO_TRANSITION_TYPE_OPTIONS,
} from './video-transition-settings';

const props = defineProps<{
  assets: any[];
  storyboards: any[];
  tracks: any[];
  initialTab?: 'preview' | 'generate' | 'editor';
  initialTrackId?: number;
  storyboardPlan?: string;
  videoMode?: string;
  videoModel?: number;
  videoRatio?: string;
}>();

const emit = defineEmits<{
  cancelVideo: [video: any];
  deleteVideo: [video: any];
  generatePrompt: [track: any];
  generateVideo: [track: any];
  openTrack: [trackId: number];
  retryVideo: [video: any, track: any];
  reorderStoryboards: [ids: number[]];
  savePrompt: [track: any];
  selectVideo: [track: any, video: any];
  exportStoryboardImages: [ids: number[]];
  exportVideo: [videoIds: number[]];
  batchGeneratePrompts: [tracks: any[]];
  batchGenerateVideos: [tracks: any[]];
  batchDownload: [tracks: any[]];
  refresh: [];
  stateChange: [state: { tab: 'preview' | 'generate' | 'editor'; trackId?: number }];
}>();

const activeTrackId = ref<number | undefined>(props.initialTrackId);
const activeTab = ref<'preview' | 'generate' | 'editor'>(props.initialTab ?? 'preview');
const previewVideo = ref<any>();
const snapshotVideo = ref<any>();
const inspectingVideoIds = ref<number[]>([]);
const snapshotRequest = computed(() => videoGenerationRequest(snapshotVideo.value));
const snapshotReferences = computed(() => videoReferenceManifest(snapshotVideo.value));
const snapshotDroppedReferences = computed(() => videoDroppedReferences(snapshotVideo.value));
const snapshotShots = computed(() => videoStructuredShots(snapshotVideo.value));

function snapshotRoleLabel(reference: any) {
  const key: string = reference?.role ?? reference?.kind ?? '';
  const labels: Record<string, string> = {
    environment_reference: '场景参考',
    first_frame: '首帧',
    last_frame: '尾帧',
    reference_image: '普通参考',
    required_subject: '必需主体',
  };
  return labels[key] || key || '参考图';
}

async function inspectVideo(video: any) {
  if (inspectingVideoIds.value.includes(video.id)) return;
  inspectingVideoIds.value.push(video.id);
  try {
    await inspectTrackVideo(video.id);
    emit('refresh');
  } finally {
    inspectingVideoIds.value = inspectingVideoIds.value.filter((id) => id !== video.id);
  }
}
const compareOpen = ref(false);
const compareIds = ref<number[]>([]);
const addReferenceOpen = ref(false);
const videoModelOptions = ref<Array<{ label: string; value: number; supportsAudio: boolean }>>([]);
const activeEditorIndex = ref(0);
const playingSequence = ref(false);
const editorPlaying = ref(false);
const editorVolume = ref(100);
const editorPlayer = ref<HTMLVideoElement>();
const selectedTrackIds = ref<number[]>([]);
const trackSelectionInitialized = ref(false);
const selectedEditorTrackIds = ref<number[]>([]);
const editorSelectionInitialized = ref(false);
const editorAutoSelectNewClips = ref(false);
const editorVideoError = ref(false);

const videoSceneGroups = computed(() =>
  groupVideoTracksByScene(props.tracks, props.storyboards, props.storyboardPlan),
);
const unassignedStoryboardCount = computed(
  () =>
    props.storyboards.filter(
      (storyboard) => !/^sc[1-9]\d*$/.test(String(storyboard.sceneKey ?? '')),
    ).length,
);
const generationStoryboardScenes = computed(() =>
  videoSceneGroups.value.filter((scene) => scene.items.length > 0),
);
const orderedTrackEntries = computed(() =>
  videoSceneGroups.value.flatMap((scene) =>
    scene.tracks.map((entry) => ({
      ...entry,
      sceneKey: scene.key,
      sceneName: scene.name,
    })),
  ),
);
const orderedTransitionTrackContexts = computed(() =>
  props.tracks.map((track) => {
    const entry = orderedTrackEntries.value.find(
      (candidate) => Number(candidate.trackId) === Number(track.id),
    );
    return {
      sceneKey: entry?.sceneKey ?? 'scene:unassigned-video-tracks',
      sceneName: entry?.sceneName ?? '未分场',
      trackId: Number(track.id),
      trackName: entry?.name ?? String(track.id),
    };
  }),
);
const activeTrack = computed(() =>
  props.tracks.find((track) => Number(track.id) === Number(activeTrackId.value)) ?? orderedTrackEntries.value[0]?.track,
);
const availableClips = computed(() =>
  orderedTrackEntries.value
    .map((entry, index) => {
      const track = entry.track;
      const videos = track.videoList ?? [];
      const video =
        videos.find((item: any) => Number(item.id) === Number(track.selectVideoId)) ??
        videos.find((item: any) => ['生成成功', '已完成'].includes(item.state));
      const src = videoUrl(video);
      return src
        ? {
            ...video,
            src,
            duration: track.duration || 5,
            index,
            trackId: entry.trackId,
            trackName: entry.name,
            sceneKey: entry.sceneKey,
            sceneName: entry.sceneName,
          }
        : undefined;
    })
    .filter(Boolean),
);
const selectedClips = computed(() =>
  availableClips.value.filter((clip: any) => selectedEditorTrackIds.value.includes(Number(clip.trackId))),
);
const availableClipScenes = computed(() =>
  videoSceneGroups.value
    .map((scene) => ({
      clips: availableClips.value.filter((clip: any) => clip.sceneKey === scene.key),
      key: scene.key,
      name: scene.name,
    }))
    .filter((scene) => scene.clips.length > 0),
);
const selectedClipScenes = computed(() =>
  availableClipScenes.value
    .map((scene) => ({
      ...scene,
      clips: scene.clips.filter((clip: any) => selectedEditorTrackIds.value.includes(Number(clip.trackId))),
    }))
    .filter((scene) => scene.clips.length > 0),
);
const selectedEditorVideoIds = computed(() => selectedClips.value.map((clip: any) => Number(clip.id)));
const totalDuration = computed(() =>
  selectedClips.value.reduce((sum, clip: any) => sum + Number(clip.duration || 0), 0),
);
const timelineTicks = computed(() => {
  const total = Math.max(0, Math.round(totalDuration.value));
  if (total === 0) return [0];
  const ticks = [0];
  for (let tick = 5; tick < total; tick += 5) ticks.push(tick);
  if (ticks.at(-1) !== total) ticks.push(total);
  return ticks;
});
const timelineContentWidth = computed(() => {
  const clipWidth = selectedClips.value.reduce(
    (sum, clip: any) => sum + timelineClipWidth(clip),
    0,
  );
  const sceneSpacing = selectedClipScenes.value.length * 12;
  return `${Math.max(560, clipWidth + Math.max(0, selectedClips.value.length - 1) * 2 + sceneSpacing)}px`;
});
const activeEditorClip = computed(() => selectedClips.value[activeEditorIndex.value]);
const activePreviewVideo = computed(() => {
  const videos = activeTrack.value?.videoList ?? [];
  const selectedId = Number(activeTrack.value?.selectVideoId);
  return videos.find((video: any) => Number(video.id) === selectedId && videoUrl(video))
    ?? videos.find((video: any) => videoUrl(video));
});
const referenceAssets = computed(() => props.assets.filter((asset) => previewUrl(asset)));
const compareVideos = computed(() => {
  const videos = activeTrack.value?.videoList ?? [];
  return compareIds.value.map((id) => videos.find((video: any) => video.id === id)).filter(Boolean);
});
const activeModelSupportsAudio = computed(() => {
  if (!activeTrack.value) return true;
  const model = videoModelOptions.value.find((item) => item.value === generation(activeTrack.value).model);
  return model?.supportsAudio ?? true;
});
const selectedTracks = computed(() => props.tracks.filter((track) => selectedTrackIds.value.includes(Number(track.id))));
const selectedTrackCount = computed(() => selectedTracks.value.length);
function successfulVideos(track: any) {
  return (track.videoList ?? []).filter((video: any) => ['生成成功', '已完成'].includes(video.state));
}

function timelineClipWidth(clip: any) {
  const duration = Math.max(1, Number(clip.duration) || 5);
  return Math.max(120, Math.round(duration * 32));
}

function timelineClipStyle(clip: any) {
  return { flex: `0 0 ${timelineClipWidth(clip)}px` };
}

function timelineSceneStyle(scene: { clips: any[] }) {
  const width = scene.clips.reduce((sum, clip) => sum + timelineClipWidth(clip), 0);
  return { flex: `0 0 ${width + Math.max(0, scene.clips.length - 1) * 2 + 8}px` };
}

function initializeEditorSelection() {
  selectedEditorTrackIds.value = availableClips.value.map((clip: any) => Number(clip.trackId));
  editorSelectionInitialized.value = true;
  editorAutoSelectNewClips.value = true;
  activeEditorIndex.value = 0;
}

function toggleEditorClip(trackId: number, checked: boolean) {
  editorAutoSelectNewClips.value = false;
  selectedEditorTrackIds.value = checked
    ? [...new Set([...selectedEditorTrackIds.value, trackId])]
    : selectedEditorTrackIds.value.filter((id) => id !== trackId);
}

function toggleAllEditorClips() {
  const allIds = availableClips.value.map((clip: any) => Number(clip.trackId));
  const selectAll = selectedEditorTrackIds.value.length !== allIds.length;
  selectedEditorTrackIds.value = selectAll ? allIds : [];
  editorAutoSelectNewClips.value = selectAll;
}

function sceneSelectionState(scene: { clips: any[] }) {
  const selectedCount = scene.clips
    .map((clip) => Number(clip.trackId))
    .filter((trackId) => selectedEditorTrackIds.value.includes(trackId)).length;
  return {
    checked: selectedCount === scene.clips.length,
    indeterminate: selectedCount > 0 && selectedCount < scene.clips.length,
  };
}

function toggleEditorScene(scene: { clips: any[] }, checked: boolean) {
  editorAutoSelectNewClips.value = false;
  const sceneTrackIds = scene.clips.map((clip) => Number(clip.trackId));
  selectedEditorTrackIds.value = checked
    ? [...new Set([...selectedEditorTrackIds.value, ...sceneTrackIds])]
    : selectedEditorTrackIds.value.filter((trackId) => !sceneTrackIds.includes(trackId));
}

function exportSelectedVideos() {
  if (selectedEditorVideoIds.value.length < 2) return;
  emit('exportVideo', selectedEditorVideoIds.value);
}

function focusEditorClip(clip: any) {
  const index = selectedClips.value.findIndex((item: any) => Number(item.trackId) === Number(clip.trackId));
  selectEditorClip(index);
}

function editorClipIndex(clip: any) {
  return selectedClips.value.findIndex((item: any) => Number(item.trackId) === Number(clip.trackId));
}

function selectEditorClip(index: number) {
  if (index < 0 || index >= selectedClips.value.length) return;
  if (activeEditorIndex.value === index) return;
  editorPlayer.value?.pause();
  editorPlaying.value = false;
  playingSequence.value = false;
  activeEditorIndex.value = index;
}

function stepEditor(delta: number) {
  const nextIndex = activeEditorIndex.value + delta;
  if (nextIndex < 0 || nextIndex >= selectedClips.value.length) return;
  selectEditorClip(nextIndex);
}

function isSelectedVideo(track: any, video: any) {
  return Number(track.selectVideoId) === Number(video.id);
}

function toggleCompare(video: any) {
  if (!['生成成功', '已完成'].includes(video.state)) return;
  if (compareIds.value.includes(video.id)) {
    compareIds.value = compareIds.value.filter((id) => id !== video.id);
  } else if (compareIds.value.length < 2) {
    compareIds.value = [...compareIds.value, video.id];
  }
  if (compareIds.value.length === 2) compareOpen.value = true;
}

function toggleTrack(id: number | undefined, checked: boolean) {
  if (id === undefined) return;
  const trackId = Number(id);
  if (!Number.isFinite(trackId)) return;
  selectedTrackIds.value = checked ? [trackId] : [];
}

function generation(track: any) {
  track.generation ??= {};
  track.generation.audio ??= true;
  const storyboards = trackStoryboards(track);
  const storyboard = storyboards[0];
  track.generation.duration = Number(track.duration || storyboard?.duration || track.generation.duration || 4);
  track.generation.mode ??= defaultVideoGenerationMode(storyboards.length, props.videoMode);
  track.generation.model ??= props.videoModel;
  track.generation.resolution ??= '1080p';
  return track.generation;
}

function transitionSettings(track: any) {
  const settings = normalizeVideoTransitionSettings(track);
  track.transitionType = settings.transitionType;
  track.framePolicy = settings.framePolicy;
  track.transitionDurationMs = settings.transitionDurationMs;
  track.trimStartMs = settings.trimStartMs;
  track.trimEndMs = settings.trimEndMs;
  return track as any;
}

function transitionDurationEnabled(track: any) {
  return transitionDurationApplies(transitionSettings(track).transitionType);
}

function trimEndMinMs(track: any) {
  return transitionSettings(track).trimStartMs + 1;
}

function previousTrackContext(track: any) {
  return previousVideoTrackContext(
    orderedTransitionTrackContexts.value,
    track?.id,
    track?.previousTrackId,
  );
}

function framePolicyOptions(track: any) {
  const hasPreviousTrack = Boolean(previousTrackContext(track));
  return VIDEO_FRAME_POLICY_OPTIONS.map((option) =>
    option.value === 'previous_tail'
      ? { ...option, disabled: !hasPreviousTrack }
      : option,
  );
}

function previousTailSourceLabel(track: any) {
  const previous = previousTrackContext(track);
  if (!previous) return '未找到可用的上一轨道';
  const trackLabel = `${previous.sceneName} · 视频轨道 ${previous.trackName} (#${previous.trackId})`;
  const previousTrack = props.tracks.find(
    (item) => Number(item.id) === previous.trackId,
  );
  const videos = Array.isArray(previousTrack?.videoList)
    ? previousTrack.videoList
    : [];
  const selectedId = Number(previousTrack?.selectVideoId);
  const sourceVideo =
    videos.find(
      (item: any) =>
        Number(item.id) === selectedId &&
        ['生成成功', '已完成'].includes(item.state) &&
        videoUrl(item),
    ) ??
    videos.find(
      (item: any) =>
        ['生成成功', '已完成'].includes(item.state) && videoUrl(item),
    );
  return sourceVideo
    ? `${trackLabel} · 视频 #${sourceVideo.id} 尾帧（生成时提取）`
    : `${trackLabel} 暂无成功视频，生成时将回退本轨分镜`;
}

function isCrossScenePreviousTail(track: any) {
  const current = orderedTransitionTrackContexts.value.find(
    (entry) => entry.trackId === Number(track?.id),
  );
  const previous = previousTrackContext(track);
  return Boolean(current && previous && current.sceneKey !== previous.sceneKey);
}

function currentFrameSourceLabel(track: any) {
  const videos = Array.isArray(track?.videoList) ? track.videoList : [];
  const selectedId = Number(track?.selectVideoId);
  const video =
    videos.find((item: any) => Number(item.id) === selectedId) ??
    videos.find((item: any) => ['生成成功', '已完成'].includes(item.state));
  const frame = videoFrameApplication(video);
  if (!frame) return '尚无结构化生成记录';
  if (frame.applied && frame.actualSource === 'previous_video_tail') {
    const trackLabel = frame.previousTrackId
      ? `轨道 #${frame.previousTrackId}`
      : '上一轨道';
    const videoLabel = frame.previousVideoId
      ? ` · 视频 #${frame.previousVideoId}`
      : '';
    return `${trackLabel}${videoLabel} 尾帧`;
  }
  if (frame.fallbackReason) return `本轨分镜（${frame.fallbackReason}）`;
  return '本轨分镜';
}

async function persistTransitionSettings(track: any) {
  const settings = transitionSettings(track);
  const previous =
    settings.framePolicy === 'previous_tail'
      ? previousTrackContext(track)
      : undefined;
  if (settings.framePolicy === 'previous_tail' && !previous) {
    track.framePolicy = 'own';
    return;
  }

  const previousTrackId = previous?.trackId;
  let trimEndMs = settings.trimEndMs;
  if (trimEndMs !== null && trimEndMs <= settings.trimStartMs) {
    trimEndMs = null;
    track.trimEndMs = null;
  }
  await updateVideoTransitionSettings({
    framePolicy: settings.framePolicy,
    id: Number(track.id),
    ...(previousTrackId === undefined ? {} : { previousTrackId }),
    transitionDurationMs: settings.transitionDurationMs,
    transitionType: settings.transitionType,
    trimEndMs,
    trimStartMs: settings.trimStartMs,
  });
  track.previousTrackId = previousTrackId;
  track.transitionSource = 'manual';
}

function trackStoryboards(track: any) {
  return storyboardsForTrack(props.storyboards, track?.id);
}

function storyboardMediaForTrack(track: any) {
  const medias = Array.isArray(track?.medias) ? track.medias : [];
  return trackStoryboards(track).map((storyboard: any) => {
    const media = medias.find((item: any) => item.sources === 'storyboard' && Number(item.id) === Number(storyboard.id));
    return {
      ...media,
      id: storyboard.id,
      src: storyboard.filePath || storyboard.src || media?.src,
      fileType: 'image',
      sources: 'storyboard',
      index: storyboard.index,
    };
  });
}

function trackMediaItems(track: any) {
  const storyboardIds = new Set(trackStoryboards(track).map((storyboard: any) => Number(storyboard.id)));
  const references = (Array.isArray(track?.medias) ? track.medias : []).filter(
    (media: any) => media.sources !== 'storyboard' && (!media.storyboardId || storyboardIds.has(Number(media.storyboardId))),
  );
  return [...storyboardMediaForTrack(track), ...references];
}

function trackSceneBindings(track: any) {
  return [
    ...new Set(
      storyboardMediaForTrack(track)
        .map((media: any) => {
          const master = media.sceneMasterName;
          const state = media.sceneStateName || media.sceneStateKey;
          return master && state ? `${master} · ${state}` : undefined;
        })
        .filter(Boolean),
    ),
  ] as string[];
}

function trackSceneBindingLabel(track: any) {
  const bindings = trackSceneBindings(track);
  if (bindings.length === 1) return bindings[0];
  if (bindings.length > 1) return `混用 ${bindings.length} 个场景状态`;
  return '未绑定场景状态';
}

function invalidSceneStoryboards(track: any) {
  return trackStoryboards(track).filter((storyboard: any) => {
    const hasImage = Boolean(storyboard.filePath || storyboard.src);
    return hasImage && storyboard.sceneConsistencyStatus !== 'ready';
  });
}

function trackStoryboardImagesReady(track: any) {
  return invalidSceneStoryboards(track).length === 0;
}

function storyboardMediaLabel(track: any, media: any) {
  const storyboards = trackStoryboards(track);
  const index = storyboards.findIndex((storyboard: any) => Number(storyboard.id) === Number(media?.id));
  const position = index >= 0 ? index : 0;
  const role = videoFrameRole(position, storyboards.length, generation(track).mode);
  const roleLabel = { first: '首帧', last: '尾帧', firstLast: '首尾帧', reference: '参考' }[role];
  return `P${position + 1} ${roleLabel}`;
}

function referenceMediaLabel(track: any, media: any) {
  const references = trackMediaItems(track).filter((item: any) => item.sources !== 'storyboard');
  const index = references.findIndex((item: any) => item === media);
  return `参考素材 ${index >= 0 ? index + 1 : 1}`;
}

function selectedStoryboard(track: any) {
  generation(track);
  return trackStoryboards(track)[0];
}

function activateTrack(track: any) {
  activeTrackId.value = Number(track.id);
  generation(track);
}

function activateTrackById(trackId: number) {
  const normalizedTrackId = Number(trackId);
  const track = props.tracks.find((item) => Number(item.id) === normalizedTrackId);
  if (track) activateTrack(track);
}

function activateStoryboardTrack(_storyboardId: number, trackId?: number) {
  if (trackId !== undefined) activateTrackById(trackId);
}

function mediaName(media: any, index: number) {
  if (media.name) return media.name;
  if (media.sources === 'storyboard') return `分镜 ${media.index ?? media.id ?? index + 1}`;
  return media.fileType === 'audio' ? `音频 ${index + 1}` : `参考素材 ${index + 1}`;
}

function mediaUrl(media: any) {
  return assetFileUrl(media?.src || media?.imageFilePath || media?.filePath || media?.fileUrl);
}

function videoUrl(video: any) {
  return assetFileUrl(video?.src || video?.filePath || video?.fileUrl);
}

function previewUrl(item: any) {
  return assetFileUrl(item?.imageFilePath || item?.filePath || item?.fileUrl || item?.src);
}

function addReferenceToTrack(track: any, asset: any) {
  const medias = track.medias ?? (track.medias = []);
  if (medias.some((media: any) => media.sources === 'assets' && Number(media.id) === Number(asset.id))) return;
  medias.push({ id: asset.id, name: asset.name, src: previewUrl(asset), fileType: 'image', sources: 'assets', storyboardId: selectedStoryboard(track)?.id });
}

function addReference(asset: any) {
  if (activeTrack.value) addReferenceToTrack(activeTrack.value, asset);
}

function removeReference(track: any, media: any) {
  if (media?.sources === 'storyboard' || !Array.isArray(track?.medias)) return;
  const index = track.medias.indexOf(media);
  if (index >= 0) track.medias.splice(index, 1);
}

function stateColor(state?: string) {
  if (state === '生成成功' || state === '已完成') return 'green';
  if (state === '生成中') return 'processing';
  if (state === '生成失败') return 'red';
  return 'default';
}

function playSequence() {
  if (!selectedClips.value.length) return;

  const player = editorPlayer.value;
  if (player && !player.paused && !player.ended) {
    player.pause();
    playingSequence.value = false;
    editorPlaying.value = false;
    return;
  }

  if (player?.ended) activeEditorIndex.value = 0;
  playingSequence.value = true;
  restoreEditorPlayer();
}

function handleEditorEnded() {
  editorPlaying.value = false;
  if (!playingSequence.value) return;
  if (activeEditorIndex.value < selectedClips.value.length - 1) {
    activeEditorIndex.value += 1;
  } else {
    playingSequence.value = false;
  }
}

function handleEditorPlay() {
  editorPlaying.value = true;
  playingSequence.value = true;
}

function handleEditorPause() {
  editorPlaying.value = false;
}

function syncEditorVolume() {
  if (editorPlayer.value) editorPlayer.value.volume = editorVolume.value / 100;
}

function restoreEditorPlayer() {
  void nextTick(() => {
    const player = editorPlayer.value;
    if (!player) return;
    editorVideoError.value = false;
    player.pause();
    player.load();
    editorPlaying.value = false;
    syncEditorVolume();
    if (playingSequence.value) {
      void player.play().catch(() => {
        playingSequence.value = false;
        editorPlaying.value = false;
      });
    }
  });
}

function handleEditorLoaded() {
  const player = editorPlayer.value;
  if (!player) return;
  editorVideoError.value = false;
  syncEditorVolume();
  if (!playingSequence.value) {
    player.currentTime = 0;
    player.pause();
    editorPlaying.value = false;
  }
}

function handleEditorError() {
  editorVideoError.value = true;
  playingSequence.value = false;
  editorPlaying.value = false;
}

watch(() => selectedClips.value.length, (clipCount) => {
  activeEditorIndex.value = Math.min(activeEditorIndex.value, Math.max(0, clipCount - 1));
});
watch(activeTab, (tab) => {
  if (tab === 'generate' || tab === 'editor') emit('refresh');
  if (tab === 'editor') {
    if (!editorSelectionInitialized.value) initializeEditorSelection();
    restoreEditorPlayer();
  }
});
watch([activeTab, activeTrackId], ([tab, trackId]) => {
  emit('stateChange', { tab, trackId: trackId === undefined ? undefined : Number(trackId) });
});
watch(() => props.initialTab, (tab) => {
  if (tab) activeTab.value = tab;
});
watch(() => props.initialTrackId, (trackId) => {
  if (trackId !== undefined) activateTrackById(trackId);
});
watch([
  () => activeEditorClip.value?.trackId,
  () => activeEditorClip.value?.src,
], () => {
  if (activeTab.value === 'editor') restoreEditorPlayer();
});
watch(() => availableClips.value.map((clip: any) => Number(clip.trackId)), (ids) => {
  const availableIds = new Set(ids);
  const nextIds = editorAutoSelectNewClips.value
    ? ids
    : selectedEditorTrackIds.value.filter((id) => availableIds.has(id));
  if (
    nextIds.length !== selectedEditorTrackIds.value.length ||
    nextIds.some((id, index) => id !== selectedEditorTrackIds.value[index])
  ) {
    selectedEditorTrackIds.value = nextIds;
  }
}, { immediate: true });
watch(selectedTrackIds, (ids) => {
  const normalizedIds = ids.map(Number).filter(Number.isFinite);
  const lastId = normalizedIds.at(-1);
  const nextIds = lastId === undefined ? [] : [lastId];
  if (nextIds.length === ids.length && nextIds.every((id, index) => id === ids[index])) return;
  selectedTrackIds.value = nextIds;
}, { immediate: true });
watch(() => orderedTrackEntries.value.map((entry) => entry.trackId), (ids) => {
  if (!ids.length) {
    selectedTrackIds.value = [];
    trackSelectionInitialized.value = false;
    activeTrackId.value = undefined;
    return;
  }
  const fallbackTrackId = ids[0];
  if (fallbackTrackId === undefined) return;
  const validSelectedIds = [...new Set(selectedTrackIds.value.map(Number).filter((id) => ids.includes(id)))];
  if (!trackSelectionInitialized.value) {
    selectedTrackIds.value = [activeTrackId.value !== undefined && ids.includes(Number(activeTrackId.value)) ? Number(activeTrackId.value) : fallbackTrackId];
    trackSelectionInitialized.value = true;
  } else if (selectedTrackIds.value.length > 0 && validSelectedIds.length === 0) {
    selectedTrackIds.value = [activeTrackId.value !== undefined && ids.includes(Number(activeTrackId.value)) ? Number(activeTrackId.value) : fallbackTrackId];
  } else {
    selectedTrackIds.value = validSelectedIds;
  }
  if (activeTrackId.value === undefined || !ids.includes(Number(activeTrackId.value))) {
    activeTrackId.value = fallbackTrackId;
  }
}, { immediate: true });
watch(activeTrackId, (id) => {
  const trackId = id === undefined ? undefined : Number(id);
  if (trackId !== undefined && Number.isFinite(trackId)) {
    selectedTrackIds.value = [trackId];
  }
  const track = props.tracks.find((item) => Number(item.id) === trackId);
  if (track) generation(track);
  compareIds.value = [];
  compareOpen.value = false;
});
watch(editorVolume, (volume) => {
  if (editorPlayer.value) editorPlayer.value.volume = volume / 100;
});

async function loadVideoModels() {
  try {
    const models = await getModelSimpleList(AiModelTypeEnum.VIDEO);
    videoModelOptions.value = models.map((model) => {
      const capabilities = model.config?.capabilities as any;
      return {
        label: model.name || model.model,
        value: model.id,
        supportsAudio: capabilities
          ? (capabilities.videoAudio ?? capabilities.audio) !== false
          : true,
      };
    });
  } catch {
    if (props.videoModel) videoModelOptions.value = [{ label: `项目模型 #${props.videoModel}`, value: props.videoModel, supportsAudio: true }];
  }
}
onMounted(loadVideoModels);
let activationCount = 0;
onActivated(() => {
  activationCount += 1;
  if (activationCount > 1) void loadVideoModels();
});
</script>

<template>
  <div class="toonflow-workbench-shell" lang="zh-CN" translate="no">
    <header class="workbench-topbar">
      <nav class="workbench-tabs" aria-label="视频工作台功能">
        <Tooltip title="分镜图预览">
          <button class="workbench-tab" :class="{ 'workbench-tab--active': activeTab === 'preview' }" type="button" @click="activeTab = 'preview'"><span>▣</span><b>分镜图预览</b></button>
        </Tooltip>
        <Tooltip title="轨道生成">
          <button class="workbench-tab" :class="{ 'workbench-tab--active': activeTab === 'generate' }" type="button" @click="activeTab = 'generate'"><span>▶</span><b>轨道生成</b></button>
        </Tooltip>
        <Tooltip title="剪辑台">
          <button class="workbench-tab" :class="{ 'workbench-tab--active': activeTab === 'editor' }" type="button" @click="activeTab = 'editor'"><span>✂</span><b>剪辑台</b></button>
        </Tooltip>
      </nav>
      <div class="workbench-nav-status">
        <span class="workbench-status-dot" />
        <span>{{ videoSceneGroups.length }} 个场次 · {{ tracks.length }} 条轨道</span>
      </div>
    </header>

    <div class="workbench-content">
      <Alert
        v-if="unassignedStoryboardCount"
        class="legacy-scene-warning"
        type="warning"
        show-icon
        :message="`${unassignedStoryboardCount} 条旧分镜尚未设置场次键`"
        description="这些分镜会安全地按硬切和本轨首帧处理。请回到分镜制作逐条补充 scN，或重新运行分镜面板 Agent，导演转场才会自动生效。"
      />
      <StoryboardQuickPreview
        v-if="activeTab === 'preview'"
        :assets="assets"
        :storyboards="storyboards"
        @export-images="emit('exportStoryboardImages', $event)"
        @reorder="emit('reorderStoryboards', $event)"
      />

      <div v-else-if="activeTab === 'generate'" class="generation-page">
        <template v-if="activeTrack">
          <section class="generation-main">
            <div class="generation-left-column">
              <div class="generation-player" :class="{ 'has-video': videoUrl(activePreviewVideo) }">
                <video v-if="videoUrl(activePreviewVideo)" :key="activePreviewVideo?.id" :src="videoUrl(activePreviewVideo)" controls disablepictureinpicture disableremoteplayback controlslist="nodownload noplaybackrate" preload="metadata" playsinline />
                <div v-else class="player-placeholder"><span>▶</span><p>生成视频后在这里预览</p></div>
              </div>

              <section class="history-section setting-block history-under-preview">
                <div class="section-heading"><div><b>历史版本</b><span>{{ successfulVideos(activeTrack).length }} 个可用候选 · 选择一个结果作为当前轨道视频</span></div><Tag :color="stateColor(activeTrack.state)">{{ activeTrack.state || '未生成' }}</Tag></div>
                <div v-if="activeTrack.videoList?.length" class="video-grid">
                    <article v-for="(video, versionIndex) in activeTrack.videoList" :key="video.id" class="video-card" :class="{ 'video-card--selected': isSelectedVideo(activeTrack, video) }">
                      <button class="video-preview" type="button" @click="previewVideo = video"><video v-if="videoUrl(video)" :src="videoUrl(video)" muted playsinline preload="metadata" /><div v-else class="video-placeholder">{{ video.state }}</div><Tag class="video-state" :color="stateColor(video.state)">{{ video.state }}</Tag><Tag v-if="video.retryOfId" class="video-retry">重试自 V{{ video.retryOfId }}</Tag></button>
                    <div class="video-actions"><span class="version-label">V{{ Number(versionIndex) + 1 }}</span><Button v-if="['生成成功','已完成'].includes(video.state)" size="small" @click="emit('selectVideo', activeTrack, video)">{{ isSelectedVideo(activeTrack, video) ? '已选中' : '选中' }}</Button><Button v-if="['生成成功','已完成'].includes(video.state)" size="small" :type="compareIds.includes(video.id) ? 'primary' : 'default'" @click="toggleCompare(video)">{{ compareIds.includes(video.id) ? '已加入对比' : '对比' }}</Button><Button v-if="video.state === '生成中'" size="small" @click="emit('cancelVideo', video)">取消</Button><Button v-if="['生成失败','已取消'].includes(video.state)" size="small" @click="emit('retryVideo', video, activeTrack)">重试</Button><Button danger size="small" type="text" @click="emit('deleteVideo', video)">删除</Button></div>
                    <div class="video-actions">
                      <Tooltip :title="videoQualityPresentation(video).detail"><Tag :color="videoQualityPresentation(video).color">{{ videoQualityPresentation(video).label }}</Tag></Tooltip>
                      <Button v-if="hasVideoGenerationSnapshot(video)" size="small" @click="snapshotVideo = video">生成快照</Button>
                      <Button v-if="videoUrl(video) && !['生成中', '已取消'].includes(video.state)" size="small" :loading="inspectingVideoIds.includes(video.id)" @click="inspectVideo(video)">检查质量</Button>
                    </div>
                    <p v-if="video.errorReason" class="video-quality-error">{{ video.errorReason }}</p>
                  </article>
                </div>
                <Empty v-else :image="Empty.PRESENTED_IMAGE_SIMPLE" description="暂无历史版本" />
              </section>
            </div>

            <aside class="generation-settings">
              <section class="setting-block prompt-block">
                <div class="section-heading"><div><b>视频提示词</b><span>描述镜头运动与画面变化</span></div><Button size="small" :loading="activeTrack.promptGenerating" :disabled="activeTrack.promptGenerating" @click="emit('generatePrompt', activeTrack)">{{ activeTrack.promptGenerating ? '生成中…' : 'AI 生成' }}</Button></div>
                <Input.TextArea v-model:value="activeTrack.prompt" :auto-size="{ minRows: 10, maxRows: 16 }" placeholder="输入视频提示词…" />
              </section>

              <section class="setting-block">
                <div class="section-heading"><div><b>轨道分镜与参考素材</b><span>按轨道和分镜顺序查看素材</span></div><Tag :color="trackSceneBindings(activeTrack).length === 1 && trackStoryboardImagesReady(activeTrack) ? 'green' : 'orange'">{{ trackSceneBindingLabel(activeTrack) }}</Tag><Button size="small" @click="addReferenceOpen = true">＋ 添加图片</Button></div>
                <Alert v-if="!trackStoryboardImagesReady(activeTrack)" type="error" show-icon :message="`${invalidSceneStoryboards(activeTrack).length} 张分镜图的场景状态已变化`" description="请回到分镜制作重新生成这些图片；旧图不能继续用于视频生成。" />
                <div v-if="trackMediaItems(activeTrack).length" class="media-strip">
                  <article v-for="(media, index) in trackMediaItems(activeTrack)" :key="`${media.sources}-${media.id}-${index}`" class="media-card">
                    <div class="media-preview">
                      <img v-if="media.fileType === 'image' && mediaUrl(media)" :src="mediaUrl(media)" :alt="mediaName(media, Number(index))" class="media-image" />
                      <div v-else-if="media.fileType === 'audio'" class="audio-preview">♪</div>
                      <div v-else class="empty-media">暂无图片</div>
                      <span class="media-order">{{ media.sources === 'storyboard' ? storyboardMediaLabel(activeTrack, media) : referenceMediaLabel(activeTrack, media) }}</span><span class="media-source">{{ media.sources === 'storyboard' ? '分镜' : '参考素材' }}</span><span v-if="media.sources === 'storyboard'" class="media-scene-state">{{ media.sceneConsistencyStatus === 'ready' ? (media.sceneStateName || media.sceneStateKey || '状态未绑定') : '状态已过期' }}</span>
                    </div>
                    <Button v-if="media.sources !== 'storyboard'" danger size="small" type="text" @click="removeReference(activeTrack, media)">删除</Button>
                  </article>
                </div>
                <Empty v-else :image="Empty.PRESENTED_IMAGE_SIMPLE" description="暂无参考素材" />
              </section>

              <section class="setting-block parameter-block">
                <div class="section-heading parameter-heading"><div><b>生成参数</b><span>决定当前轨道的视频输出</span></div><Tag color="blue">{{ videoRatio || '16:9' }}</Tag></div>
                <div class="parameter-grid">
                  <label><span>视频模型</span><Select v-model:value="generation(activeTrack).model" :options="videoModelOptions" placeholder="选择视频模型" size="small" /></label>
                  <label><span>生成模式</span><Select v-model:value="generation(activeTrack).mode" :options="[{label:'纯文本',value:'text'},{label:'单图首帧',value:'singleImage'},{label:'首尾帧',value:'startEndRequired'},{label:'首帧必填、尾帧可选',value:'endFrameOptional'},{label:'尾帧必填、首帧可选',value:'startFrameOptional'}]" size="small" /></label>
                  <label><span>入场过渡</span><Select v-model:value="transitionSettings(activeTrack).transitionType" :options="VIDEO_TRANSITION_TYPE_OPTIONS" size="small" @change="persistTransitionSettings(activeTrack)" /></label>
                  <label><span>首帧来源</span><Select v-model:value="transitionSettings(activeTrack).framePolicy" :options="framePolicyOptions(activeTrack)" size="small" @change="persistTransitionSettings(activeTrack)" /></label>
                  <label><span>过渡时长</span><InputNumber v-model:value="transitionSettings(activeTrack).transitionDurationMs" :disabled="!transitionDurationEnabled(activeTrack)" :max="10000" :min="0" :step="50" size="small" @change="persistTransitionSettings(activeTrack)" /><span>{{ transitionDurationEnabled(activeTrack) ? '毫秒 · 叠化交叉淡化音画，声音桥接提前引入下一轨声音' : '毫秒 · 当前过渡类型导出时为硬切，时长不生效' }}</span></label>
                  <label><span>裁剪开始</span><InputNumber v-model:value="transitionSettings(activeTrack).trimStartMs" :min="0" :step="100" size="small" @change="persistTransitionSettings(activeTrack)" /><span>毫秒 · 导出时从该时间点开始播放</span></label>
                  <label><span>裁剪结束</span><InputNumber v-model:value="transitionSettings(activeTrack).trimEndMs" :min="trimEndMinMs(activeTrack)" :step="100" placeholder="播放到结尾" size="small" @change="persistTransitionSettings(activeTrack)" /><span>毫秒 · 留空则播放到片段结尾</span></label>
                  <label><span>分辨率</span><Select v-model:value="generation(activeTrack).resolution" :options="[{label:'720p',value:'720p'},{label:'1080p',value:'1080p'}]" size="small" /></label>
                  <label class="parameter-field--readonly"><span>分镜时长</span><span class="duration-readonly">{{ generation(activeTrack).duration }} 秒 · 来自分镜</span></label>
                  <div
                    v-if="transitionSettings(activeTrack).framePolicy === 'previous_tail'"
                    class="frame-source-note"
                    :class="{ 'frame-source-note--cross-scene': isCrossScenePreviousTail(activeTrack) }"
                  >
                    <span><b>上一尾帧来源：</b>{{ previousTailSourceLabel(activeTrack) }}</span>
                    <span v-if="isCrossScenePreviousTail(activeTrack)" class="frame-source-warning">跨场提醒：当前轨道将继承上一场视频尾帧，请确认不是硬切换场。</span>
                  </div>
                  <div class="transition-setting-meta">
                    <span>设置来源：{{ transitionSourceLabel(activeTrack.transitionSource) }}</span>
                    <span>当前视频首帧：{{ currentFrameSourceLabel(activeTrack) }}</span>
                  </div>
                </div>
              <div class="generate-actions"><label v-if="activeModelSupportsAudio" class="audio-setting"><span>生成音频</span><Switch v-model:checked="generation(activeTrack).audio" /></label><span v-else class="audio-unavailable">当前视频模型不支持原生音频</span><Button type="primary" :disabled="(!activeTrack.prompt?.trim() && !selectedStoryboard(activeTrack)?.videoDesc?.trim()) || activeTrack.videoGenerating || !trackStoryboardImagesReady(activeTrack)" :loading="activeTrack.videoGenerating || activeTrack.state === '生成中'" @click="emit('generateVideo', activeTrack)">生成视频</Button></div>
              </section>

            </aside>
          </section>

          <section class="track-filmstrip setting-block">
                <div class="section-heading"><div><b>选择轨道</b><span>{{ selectedTrackCount }} 项已选（单选）</span></div><div class="batch-track-actions"><Button size="small" :disabled="!selectedTracks.length || selectedTracks.some((track:any) => track.promptGenerating)" :loading="selectedTracks.some((track:any) => track.promptGenerating)" @click="emit('batchGeneratePrompts', selectedTracks)">生成提示词</Button><Button size="small" :disabled="!selectedTracks.length || selectedTracks.some((track:any) => track.videoGenerating || !trackStoryboardImagesReady(track))" :loading="selectedTracks.some((track:any) => track.videoGenerating)" @click="emit('batchGenerateVideos', selectedTracks)">生成视频</Button><Button size="small" :disabled="!selectedTracks.length" @click="emit('batchDownload', selectedTracks)">下载视频</Button><Button size="small" @click="emit('openTrack', activeTrack.id)">调整分镜</Button></div></div>
            <div class="generation-scene-strips">
              <section v-for="scene in generationStoryboardScenes" :key="scene.key" class="generation-scene-strip">
                <header class="generation-scene-heading">
                  <b>{{ scene.name }}</b>
                  <span>{{ scene.items.length }} 个分镜 · {{ scene.tracks.length }} 条视频轨道</span>
                </header>
                <StoryboardTrackStrip
                  :active-track-id="Number(activeTrack.id)"
                  :preserve-order="true"
                  :selected-track-ids="selectedTrackIds"
                  :show-track-checkbox="true"
                  :storyboards="scene.items"
                  @storyboard-click="activateStoryboardTrack"
                  @track-click="activateTrackById"
                  @track-toggle="toggleTrack"
                />
              </section>
            </div>
          </section>
        </template>
        <Empty v-else :image="Empty.PRESENTED_IMAGE_SIMPLE" description="分镜生成后进入视频工作台" />
      </div>

      <div v-else class="editor-pane">
        <section class="editor-workspace">
          <aside class="editor-media-library">
            <header><div class="editor-panel-title"><b>视频素材</b><small>选择参与合成的片段</small></div><div class="editor-media-header-actions"><Tag>{{ selectedClips.length }} / {{ availableClips.length }}</Tag><Button size="small" type="link" :disabled="!availableClips.length" @click="toggleAllEditorClips">{{ selectedClips.length === availableClips.length ? '取消全选' : '全选' }}</Button></div></header>
            <section v-for="scene in availableClipScenes" :key="scene.key" class="editor-scene-library">
              <header>
                <Checkbox
                  :checked="sceneSelectionState(scene).checked"
                  :indeterminate="sceneSelectionState(scene).indeterminate"
                  @change="toggleEditorScene(scene, $event.target.checked)"
                >
                  {{ scene.name }}
                </Checkbox>
                <small>{{ scene.clips.length }} 条</small>
              </header>
              <button v-for="clip in scene.clips" :key="clip.trackId" :class="{ active: activeEditorClip?.trackId === clip.trackId }" @click="focusEditorClip(clip)">
                <Checkbox class="editor-clip-check" :checked="selectedEditorTrackIds.includes(Number(clip.trackId))" @click.stop @change="toggleEditorClip(Number(clip.trackId), $event.target.checked)" />
                <video :src="clip.src" muted playsinline preload="metadata" disablepictureinpicture /><span>视频轨道 {{ clip.trackName }}.mp4<small>{{ clip.duration }}s</small></span>
              </button>
            </section>
            <Empty v-if="!availableClips.length" :image="Empty.PRESENTED_IMAGE_SIMPLE" description="暂无视频素材" />
            <Empty v-else-if="!selectedClips.length" :image="Empty.PRESENTED_IMAGE_SIMPLE" description="请选择要合成的视频" />
          </aside>

          <main class="editor-stage">
            <div class="editor-stage-heading"><div><b>片段预览</b><span v-if="activeEditorClip">{{ activeEditorClip.sceneName }} · 视频轨道 {{ activeEditorClip.trackName }} · {{ activeEditorClip.duration }} 秒</span></div><Tag v-if="activeEditorClip" color="blue">{{ activeEditorIndex + 1 }} / {{ selectedClips.length }}</Tag></div>
            <div class="editor-canvas">
              <video v-if="activeEditorClip?.src" :key="activeEditorClip.trackId" ref="editorPlayer" :src="activeEditorClip.src" controls playsinline preload="metadata" @ended="handleEditorEnded" @loadeddata="handleEditorLoaded" @error="handleEditorError" @play="handleEditorPlay" @pause="handleEditorPause" />
              <div v-if="editorVideoError" class="editor-video-error"><span>!</span><p>视频资源加载失败，请稍后重试</p><Button size="small" @click="restoreEditorPlayer">重新加载</Button></div>
              <div v-else-if="!activeEditorClip?.src" class="editor-empty"><span class="editor-play">▶</span><p>请先在轨道生成中选中视频</p><Button type="primary" @click="activeTab = 'generate'">进入轨道生成</Button></div>
            </div>
            <div class="editor-transport"><Button shape="circle" :disabled="activeEditorIndex === 0" title="上一段" @click="stepEditor(-1)">◀</Button><Button shape="circle" type="primary" :disabled="!selectedClips.length" :title="editorPlaying ? '暂停播放' : '播放当前片段'" @click="playSequence">{{ editorPlaying ? 'Ⅱ' : '▶' }}</Button><Button shape="circle" :disabled="activeEditorIndex >= selectedClips.length - 1" title="下一段" @click="stepEditor(1)">▶</Button><span class="editor-transport-status"><b>片段 {{ activeEditorIndex + 1 }} / {{ selectedClips.length || 0 }}</b><small>{{ activeEditorClip?.duration || 0 }} 秒</small></span></div>
          </main>

          <aside class="editor-properties">
            <header><div class="editor-panel-title"><b>视频属性</b><small>当前片段设置</small></div></header>
            <label><span>画面比例</span><b>{{ videoRatio || '16:9' }}</b></label>
            <label><span>分辨率</span><b>1080p</b></label>
            <label><span>当前场次</span><b>{{ activeEditorClip?.sceneName || '—' }}</b></label>
            <label><span>当前片段</span><b>{{ activeEditorClip?.duration || 0 }}s</b></label>
            <label><span>成片时长</span><b>{{ totalDuration }}s</b></label>
            <div class="volume-control"><span>音量 {{ editorVolume }}%</span><Slider v-model:value="editorVolume" :min="0" :max="100" /></div>
            <Button type="primary" :disabled="selectedClips.length < 2" @click="exportSelectedVideos">按场次合并导出整集</Button>
            <small v-if="selectedClips.length < 2" class="editor-export-hint">至少选择 2 个视频片段</small>
            <small v-else class="editor-export-hint">将按场次和场内轨道顺序合并</small>
          </aside>
        </section>

        <section class="timeline-panel editor-timeline">
          <div class="timeline-toolbar"><b>主轨道（视频）</b><span>{{ selectedClips.length }} 个片段 · {{ totalDuration }} 秒</span></div>
          <div class="timeline-lane">
            <div class="timeline-lane-label">视频</div>
            <div class="timeline-scroll">
              <div class="timeline-ruler" :style="{ width: timelineContentWidth }"><span v-for="tick in timelineTicks" :key="tick">{{ tick }}s</span></div>
              <div v-if="selectedClips.length" class="clip-track timeline-scene-track" :style="{ width: timelineContentWidth }">
                <section v-for="scene in selectedClipScenes" :key="scene.key" class="timeline-scene-group" :style="timelineSceneStyle(scene)">
                  <header>{{ scene.name }}</header>
                  <div class="timeline-scene-clips">
                    <button v-for="clip in scene.clips" :key="clip.trackId" class="timeline-clip" :class="{ active: activeEditorClip?.trackId === clip.trackId }" :style="timelineClipStyle(clip)" type="button" @click="focusEditorClip(clip)"><video :src="clip.src" muted playsinline preload="metadata" disablepictureinpicture /><span>{{ editorClipIndex(clip) + 1 }}. 视频轨道 {{ clip.trackName }} · {{ clip.duration }}s</span></button>
                  </div>
                </section>
              </div>
              <div v-else class="empty-track">暂无可拼接视频片段</div>
            </div>
          </div>
        </section>
      </div>
    </div>

    <Modal root-class-name="toon-overlay" v-model:open="previewVideo" width="76vw" :footer="null" title="视频预览" destroy-on-close>
          <video v-if="videoUrl(previewVideo)" :key="previewVideo?.id" :src="videoUrl(previewVideo)" class="preview-player" controls disablepictureinpicture disableremoteplayback controlslist="nodownload noplaybackrate" preload="metadata" playsinline />
    </Modal>
    <Modal root-class-name="toon-overlay" v-model:open="snapshotVideo" width="920px" :footer="null" title="视频生成快照" destroy-on-close>
      <div class="generation-snapshot">
        <div class="snapshot-summary">
          <Tag color="blue">快照 v{{ snapshotRequest.version ?? 1 }}</Tag>
          <Tag>{{ snapshotRequest.model || '模型未记录' }}</Tag>
          <Tag>{{ snapshotRequest.payload?.mode || '模式未记录' }}</Tag>
          <span>视频任务 #{{ snapshotVideo?.id }}</span>
        </div>
        <section>
          <header><b>实际参考图</b><span>{{ snapshotReferences.length }} 张 · 顺序与供应商请求一致</span></header>
          <div v-if="snapshotReferences.length" class="snapshot-reference-list">
            <article v-for="reference in snapshotReferences" :key="`${reference.index}-${reference.url}`">
              <img v-if="reference.url" :src="assetFileUrl(String(reference.url))" :alt="reference.name || `参考图 ${reference.index}`" loading="lazy" />
              <div><b>@图{{ reference.index }} · {{ reference.name || '非资产帧' }}</b><span>{{ snapshotRoleLabel(reference) }}</span><small v-if="reference.assetId">资产 #{{ reference.assetId }} · 图片 #{{ reference.imageId ?? '—' }} · {{ reference.assetType }}</small><small class="snapshot-url">{{ reference.url || 'URL 未记录' }}</small></div>
            </article>
          </div>
          <Empty v-else :image="Empty.PRESENTED_IMAGE_SIMPLE" description="该任务没有参考图快照" />
          <div v-if="snapshotDroppedReferences.length" class="snapshot-dropped-references">
            <header><b>已舍弃的参考图</b><span>{{ snapshotDroppedReferences.length }} 张 · 按上限策略未发送</span></header>
            <ul>
              <li v-for="(reference, index) in snapshotDroppedReferences" :key="`${reference.name ?? reference.filePath}-${index}`">
                <b>{{ reference.name || reference.filePath || '未命名参考' }}</b>
                <small>{{ reference.reason || '超出参考图上限，按优先级舍弃' }}</small>
              </li>
            </ul>
          </div>
        </section>
        <section>
          <header><b>结构化镜头</b><span>{{ snapshotShots.length }} 个镜头</span></header>
          <div v-if="snapshotShots.length" class="snapshot-shot-list">
            <article v-for="shot in snapshotShots" :key="shot.storyboardId ?? shot.sequence">
              <div class="snapshot-shot-heading"><b>镜头 {{ shot.sequence ?? '—' }}</b><Tag>{{ shot.durationSeconds ?? '—' }} 秒</Tag><span>{{ shot.sceneKey || '未分场' }}<template v-if="shot.sceneStateKey"> · {{ shot.sceneStateKey }}</template></span></div>
              <p>{{ shot.description || '无画面描述' }}</p>
              <div v-if="shot.references?.length" class="snapshot-shot-references"><Tag v-for="reference in shot.references" :key="reference.assetId">{{ reference.index ? `@图${reference.index} · ` : '' }}{{ reference.name || `资产 #${reference.assetId}` }}</Tag></div>
            </article>
          </div>
          <Empty v-else :image="Empty.PRESENTED_IMAGE_SIMPLE" description="旧任务尚无结构化镜头快照" />
        </section>
        <section>
          <header><b>供应商请求参数</b><span>凭据不会写入生成快照</span></header>
          <pre>{{ JSON.stringify(snapshotRequest.payload ?? {}, null, 2) }}</pre>
        </section>
      </div>
    </Modal>
    <Modal root-class-name="toon-overlay" v-model:open="compareOpen" width="90vw" title="候选版本对比" :footer="null" destroy-on-close>
      <div class="compare-grid">
        <article v-for="video in compareVideos" :key="video.id" class="compare-item">
          <div class="compare-heading"><b>候选版本 #{{ video.id }}</b><Tag :color="isSelectedVideo(activeTrack, video) ? 'blue' : 'default'">{{ isSelectedVideo(activeTrack, video) ? '当前版本' : '候选' }}</Tag></div>
          <video v-if="videoUrl(video)" :src="videoUrl(video)" controls preload="metadata" playsinline />
          <Button v-if="!isSelectedVideo(activeTrack, video)" type="primary" @click="emit('selectVideo', activeTrack, video); compareOpen = false">设为当前版本</Button>
        </article>
      </div>
    </Modal>
    <Modal root-class-name="toon-overlay" v-model:open="addReferenceOpen" :title="`添加项目资产 · 当前为轨道 ${Math.max(0, tracks.findIndex((track) => track.id === activeTrack?.id) + 1)}`" width="760px" :footer="null" destroy-on-close>
      <div class="asset-picker">
        <button v-for="asset in referenceAssets" :key="asset.id" type="button" @click="addReference(asset)">
          <img :src="previewUrl(asset)" :alt="asset.name" loading="lazy" decoding="async" /><span>{{ asset.name }}</span><b>＋</b>
        </button>
      </div>
    </Modal>
  </div>
</template>

<style scoped>
.toonflow-workbench-shell { display: grid; width: 100%; height: 100%; min-width: 0; min-height: 0; overflow: hidden; grid-template-rows: 64px minmax(0, 1fr); background: var(--ant-color-bg-container); }
.workbench-topbar { display: grid; min-width: 0; align-items: center; border-bottom: 1px solid var(--ant-color-border-secondary); grid-template-columns: minmax(170px, 1fr) auto minmax(170px, 1fr); padding: 0 18px; background: var(--ant-color-bg-container); }
.workbench-nav-status { display: flex; grid-column: 3; align-items: center; gap: 6px; justify-self: end; color: var(--ant-color-text-tertiary); font-size: 11px; }.workbench-status-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--ant-color-success); box-shadow: 0 0 0 3px var(--ant-color-success-bg); }
.workbench-tabs { display: flex; grid-column: 2; height: 100%; align-items: stretch; gap: 4px; justify-self: center; }.workbench-tabs button { position: relative; display: flex; min-width: 96px; align-items: center; justify-content: center; gap: 6px; padding: 0 12px; border: 0; color: var(--ant-color-text-secondary); background: transparent; cursor: pointer; }.workbench-tabs button::after { position: absolute; right: 10px; bottom: 0; left: 10px; height: 3px; border-radius: 3px 3px 0 0; background: transparent; content: ''; }.workbench-tabs button:hover { color: var(--ant-color-primary); background: var(--ant-color-fill-quaternary); }.workbench-tabs button.active { color: var(--ant-color-primary); }.workbench-tabs button.active::after { background: var(--ant-color-primary); }.workbench-tabs span { font-size: 16px; }.workbench-tabs b { font-size: 12px; font-weight: 500; }
.workbench-tabs button.active { color: var(--ant-color-primary); background: var(--ant-color-primary-bg); }
.workbench-tabs button.active span { color: var(--ant-color-primary); }
.workbench-tabs button.active b { color: var(--ant-color-primary); font-weight: 600; }
.workbench-tabs .workbench-tab--active { color: var(--ant-color-primary) !important; background: rgb(22 119 255 / 12%) !important; }
.workbench-tabs .workbench-tab--active::after { background: var(--ant-color-primary) !important; }
.workbench-tabs .workbench-tab--active span, .workbench-tabs .workbench-tab--active b { color: var(--ant-color-primary) !important; }
.workbench-tabs .workbench-tab--active b { font-weight: 700; }
.legacy-scene-warning { margin: 12px 16px 0; }
.workbench-content { min-width: 0; min-height: 0; overflow-x: hidden; overflow-y: auto; background: var(--ant-color-bg-layout); }
.toonflow-workbench { display: grid; width: 100%; height: 100%; grid-template-columns: 240px minmax(0, 1fr); overflow: hidden; background: var(--ant-color-bg-layout); }
.editor-pane { width: 100%; height: 100%; overflow-y: auto; padding: 20px 28px; background: var(--ant-color-bg-layout); }
.pane-heading { display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; }.pane-heading h3 { margin: 0; font-size: 18px; }.pane-heading p { margin: 4px 0 0; color: var(--ant-color-text-tertiary); font-size: 12px; }
.editor-preview { display: grid; width: min(820px, 100%); overflow: hidden; aspect-ratio: 16 / 9; margin: 0 auto 18px; border-radius: 9px; color: #fff; background: #0b0f19; place-items: center; }.editor-preview video { width: 100%; height: 100%; object-fit: contain; }.editor-empty { display: grid; gap: 10px; text-align: center; place-items: center; }.editor-empty p { margin: 0; color: #94a3b8; }.editor-play { display: grid; width: 58px; height: 58px; border-radius: 50%; background: rgb(255 255 255 / 12%); place-items: center; }
.editor-workspace { display: grid; min-height: 480px; overflow: hidden; border: 1px solid var(--ant-color-border-secondary); border-radius: 8px 8px 0 0; grid-template-columns: 210px minmax(0, 1fr) 240px; background: var(--ant-color-bg-container); }
.editor-media-library, .editor-properties { min-width: 0; padding: 12px; background: var(--ant-color-bg-container); }.editor-media-library { overflow-y: auto; border-right: 1px solid var(--ant-color-border-secondary); }.editor-properties { border-left: 1px solid var(--ant-color-border-secondary); }.editor-media-library header, .editor-properties header { display: flex; height: 34px; align-items: center; justify-content: space-between; margin-bottom: 10px; }
.editor-media-library > button { display: grid; width: 100%; min-width: 0; align-items: center; gap: 8px; margin-bottom: 8px; padding: 5px; border: 1px solid transparent; border-radius: 6px; text-align: left; background: var(--ant-color-fill-tertiary); cursor: pointer; grid-template-columns: 68px minmax(0, 1fr); }.editor-media-library > button.active { border-color: var(--ant-color-primary); background: var(--ant-color-primary-bg); }.editor-media-library video { width: 68px; height: 48px; object-fit: cover; background: #000; }.editor-media-library button span { overflow: hidden; font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }.editor-media-library small { display: block; color: var(--ant-color-text-tertiary); }
.editor-media-library > header { min-width: 0; }
.editor-media-header-actions { display: flex; min-width: 0; align-items: center; gap: 4px; }
.editor-media-library > button { position: relative; }
.editor-clip-check { position: absolute; z-index: 2; top: 6px; left: 6px; padding: 3px; border-radius: 4px; background: rgb(255 255 255 / 92%); }
.editor-stage { display: grid; min-width: 0; padding: 18px; background: var(--ant-color-bg-layout); grid-template-rows: minmax(0, 1fr) 48px; }.editor-canvas { display: grid; width: 100%; min-height: 0; align-self: center; overflow: hidden; aspect-ratio: 16 / 9; background: #000; place-items: center; }.editor-canvas video { display: block; width: 100%; height: 100%; object-fit: contain; }.editor-transport { display: flex; align-items: center; justify-content: center; gap: 12px; color: var(--ant-color-text-secondary); }.editor-transport span { margin-left: 8px; font-size: 12px; }
.editor-video-error { display: grid; gap: 8px; color: var(--toon-line); text-align: center; place-items: center; }.editor-video-error span { display: grid; width: 42px; height: 42px; border-radius: 50%; color: #fff; background: var(--ant-color-error); font-size: 24px; place-items: center; }.editor-video-error p { margin: 0; }
.editor-properties label { display: flex; align-items: center; justify-content: space-between; padding: 10px 0; border-bottom: 1px solid var(--ant-color-border-secondary); }.editor-properties label span, .volume-control > span { color: var(--ant-color-text-secondary); font-size: 12px; }.volume-control { margin: 18px 0; }.editor-properties > .ant-btn { width: 100%; }
.editor-export-hint { display: block; margin-top: 8px; color: var(--ant-color-text-tertiary); font-size: 11px; text-align: center; }
.editor-timeline { border-top: 0; border-radius: 0 0 8px 8px; }.timeline-clip.active { border-color: #fff; box-shadow: 0 0 0 2px var(--ant-color-primary); }
.timeline-panel { padding: 14px; border: 1px solid var(--ant-color-border-secondary); border-radius: 9px; background: var(--ant-color-bg-container); }.timeline-toolbar, .timeline-ruler { display: flex; align-items: center; justify-content: space-between; }.timeline-toolbar span { color: var(--ant-color-text-tertiary); font-size: 12px; }.timeline-ruler { margin: 12px 0 5px; padding-left: 96px; color: var(--ant-color-text-tertiary); font-size: 10px; }.clip-track { display: flex; min-height: 74px; gap: 2px; padding: 6px 6px 6px 96px; border-radius: 5px; background: var(--ant-color-fill-tertiary); }.timeline-clip { position: relative; min-width: 90px; overflow: hidden; padding: 0; border: 2px solid var(--ant-color-primary); border-radius: 4px; color: #fff; background: var(--toon-ink); cursor: pointer; }.timeline-clip video { width: 100%; height: 100%; object-fit: cover; opacity: 0.7; }.timeline-clip span { position: absolute; bottom: 3px; left: 5px; font-size: 10px; text-shadow: 0 1px 2px #000; }.empty-track { display: grid; height: 74px; margin-left: 96px; color: var(--ant-color-text-tertiary); background: var(--ant-color-fill-tertiary); place-items: center; }.subtitle-track { display: flex; height: 42px; align-items: center; gap: 22px; margin-top: 5px; padding: 0 12px; border-radius: 5px; background: var(--ant-color-fill-tertiary); }.subtitle-track b { width: 72px; }.subtitle-track span { flex: 1; padding: 4px 8px; border-radius: 3px; color: var(--ant-color-text-secondary); font-size: 11px; background: var(--ant-color-warning-bg); }
.track-sidebar { overflow-y: auto; padding: 12px; border-right: 1px solid var(--ant-color-border-secondary); background: var(--ant-color-bg-container); }
.sidebar-title { display: flex; align-items: center; justify-content: space-between; padding: 4px 4px 12px; }
.sidebar-title span { display: grid; width: 24px; height: 24px; border-radius: 12px; color: var(--ant-color-text-secondary); background: var(--ant-color-fill-secondary); place-items: center; }
.track-tab { display: flex; width: 100%; align-items: center; gap: 10px; margin-bottom: 8px; padding: 10px; border: 1px solid transparent; border-radius: 8px; color: inherit; text-align: left; background: transparent; cursor: pointer; }
.track-tab:hover { background: var(--ant-color-fill-tertiary); }
.track-tab.active { border-color: var(--ant-color-primary-border); background: var(--ant-color-primary-bg); }
.track-number { display: grid; width: 28px; height: 28px; flex: 0 0 28px; border-radius: 6px; color: var(--ant-color-text-secondary); background: var(--ant-color-fill-secondary); place-items: center; }
.track-tab.active .track-number { color: #fff; background: var(--ant-color-primary); }
.track-summary { display: grid; min-width: 0; flex: 1; }
.track-summary small { overflow: hidden; color: var(--ant-color-text-tertiary); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.state-dot { width: 7px; height: 7px; flex: 0 0 7px; border-radius: 50%; background: var(--ant-color-text-quaternary); }
.state-green { background: var(--ant-color-success); }.state-processing { background: var(--ant-color-primary); }.state-red { background: var(--ant-color-error); }
.track-workspace { overflow-y: auto; padding: 20px; }
.workspace-header, .section-heading, .prompt-actions, .video-actions { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.workspace-header h3 { margin: 0; font-size: 18px; }.workspace-header p { margin: 3px 0 0; color: var(--ant-color-text-tertiary); font-size: 12px; }
.workbench-section { margin-top: 16px; padding: 16px; border: 1px solid var(--ant-color-border-secondary); border-radius: 10px; background: var(--ant-color-bg-container); }
.section-heading { margin-bottom: 12px; }.section-heading > div { display: flex; align-items: baseline; gap: 10px; }.section-heading span { color: var(--ant-color-text-tertiary); font-size: 12px; }
.media-strip { display: flex; gap: 10px; overflow-x: auto; padding-bottom: 4px; }
.media-card { width: 132px; min-width: 0; flex: 0 0 132px; }.media-preview { position: relative; display: grid; width: 100%; height: auto; overflow: hidden; aspect-ratio: 16 / 9; border-radius: 7px; background: var(--ant-color-fill-secondary); place-items: center; }
.media-preview .media-image, .media-preview video { display: block; width: 100%; height: 100%; }
.media-preview .media-image { max-width: 100%; max-height: 100%; object-fit: scale-down !important; object-position: center; }
.media-preview video { object-fit: cover; }
.media-order, .media-source, .media-scene-state { position: absolute; padding: 1px 6px; border-radius: 10px; color: #fff; font-size: 10px; background: rgb(0 0 0 / 60%); }.media-order, .media-source { top: 6px; }.media-order { left: 6px; }.media-source { right: 6px; }.media-scene-state { right: 6px; bottom: 6px; }
.audio-preview, .empty-media { display: grid; height: 100%; color: var(--ant-color-text-tertiary); place-items: center; }.audio-preview { font-size: 28px; }
.duration-readonly { color: var(--ant-color-text-secondary); font-size: 12px; }
.media-name { overflow: hidden; margin-top: 6px; font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.prompt-section :deep(textarea) { resize: none; }.prompt-actions { margin-top: 12px; }.prompt-actions > span { color: var(--ant-color-text-secondary); font-size: 12px; }
.video-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(230px, 1fr)); gap: 12px; }
.video-card { overflow: hidden; border: 1px solid var(--ant-color-border-secondary); border-radius: 8px; }
.video-card--selected { border-color: var(--ant-color-primary); box-shadow: 0 0 0 2px var(--ant-color-primary-bg); }
.compare-grid { display:grid; gap:16px; grid-template-columns:repeat(2,minmax(0,1fr)); }.compare-item { padding:12px; border:1px solid var(--ant-color-border-secondary); border-radius:8px; }.compare-heading { display:flex; align-items:center; justify-content:space-between; margin-bottom:10px; }.compare-item video { display:block; width:100%; max-height:60vh; aspect-ratio:16/9; margin-bottom:12px; background:#000; object-fit:contain; }
.generation-snapshot { display: grid; gap: 16px; }
.snapshot-summary { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.snapshot-summary > span { margin-left: auto; color: var(--ant-color-text-tertiary); font-size: 12px; }
.generation-snapshot > section { padding: 14px; border: 1px solid var(--ant-color-border-secondary); border-radius: 8px; background: var(--ant-color-bg-layout); }
.generation-snapshot > section > header { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; margin-bottom: 10px; }
.generation-snapshot > section > header span { color: var(--ant-color-text-tertiary); font-size: 12px; }
.snapshot-reference-list, .snapshot-shot-list { display: grid; gap: 8px; }
.snapshot-reference-list { grid-template-columns: repeat(2, minmax(0, 1fr)); }
.snapshot-reference-list article { display: grid; min-width: 0; gap: 10px; padding: 8px; border: 1px solid var(--ant-color-border-secondary); border-radius: 7px; background: var(--ant-color-bg-container); grid-template-columns: 96px minmax(0, 1fr); }
.snapshot-reference-list img { width: 96px; height: 64px; border-radius: 5px; background: #000; object-fit: contain; }
.snapshot-reference-list article > div { display: grid; min-width: 0; align-content: start; gap: 3px; }
.snapshot-reference-list span, .snapshot-reference-list small { color: var(--ant-color-text-secondary); font-size: 11px; }
.snapshot-url { overflow: hidden; color: var(--ant-color-text-tertiary) !important; text-overflow: ellipsis; white-space: nowrap; }
.snapshot-dropped-references { margin-top: 10px; padding: 10px; border: 1px dashed var(--ant-color-border); border-radius: 8px; }
.snapshot-dropped-references header { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
.snapshot-dropped-references header span { color: var(--ant-color-text-tertiary); font-size: 11px; }
.snapshot-dropped-references ul { margin: 8px 0 0; padding: 0; list-style: none; display: grid; gap: 4px; }
.snapshot-dropped-references li { display: grid; gap: 2px; }
.snapshot-dropped-references small { color: var(--ant-color-text-tertiary); font-size: 11px; }
.snapshot-shot-list article { padding: 10px; border-left: 3px solid var(--ant-color-primary); border-radius: 5px; background: var(--ant-color-bg-container); }
.snapshot-shot-heading { display: flex; align-items: center; gap: 8px; }
.snapshot-shot-heading > span:last-child { margin-left: auto; color: var(--ant-color-text-tertiary); font-size: 11px; }
.snapshot-shot-list p { margin: 8px 0; color: var(--ant-color-text-secondary); line-height: 1.6; white-space: pre-wrap; }
.snapshot-shot-references { display: flex; gap: 4px; flex-wrap: wrap; }
.generation-snapshot pre { max-height: 320px; margin: 0; padding: 10px; overflow: auto; border-radius: 6px; color: var(--ant-color-text-secondary); background: var(--ant-color-fill-tertiary); font-size: 11px; white-space: pre-wrap; word-break: break-all; }
.video-preview { position: relative; display: grid; width: 100%; overflow: hidden; aspect-ratio: 16 / 9; padding: 0; border: 0; color: #fff; background: var(--toon-ink); cursor: pointer; place-items: center; }.video-preview video { width: 100%; height: 100%; object-fit: cover; }
.video-placeholder { display: grid; gap: 8px; color: var(--toon-line); font-size: 12px; place-items: center; }.video-state { position: absolute; top: 8px; left: 8px; }.play-mark { position: absolute; display: grid; width: 38px; height: 38px; border-radius: 50%; background: rgb(0 0 0 / 50%); place-items: center; }
.generating-ring { width: 24px; height: 24px; border: 2px solid rgb(255 255 255 / 25%); border-top-color: #fff; border-radius: 50%; animation: spin 0.9s linear infinite; }
.error-reason { margin: 8px 10px 0; color: var(--ant-color-error); font-size: 11px; line-height: 1.4; }.video-actions { justify-content: flex-end; padding: 8px; }.version-label { margin-right: auto; color: var(--ant-color-text-tertiary); font-size: 11px; }
.empty-workbench { grid-column: 1 / -1; align-self: center; }.preview-player { display: block; width: 100%; max-height: 72vh; background: #000; }
.generation-page { display: grid; width: 100%; max-width: 100%; min-width: 0; overflow-x: hidden; gap: 0; padding: 0; box-sizing: border-box; grid-template-rows: auto auto auto; }
.generation-main { display: grid; width: 100%; max-width: 100%; min-width: 0; align-items: stretch; overflow: hidden; gap: 0; grid-template-columns: minmax(0, 56%) minmax(0, 44%); }
.generation-player { position: relative; display: grid; width: calc(100% - 40px); max-width: 100%; min-width: 0; height: auto; min-height: 0; align-self: start; overflow: hidden; aspect-ratio: 16 / 9; margin: 20px; border-radius: 8px; color: #fff; background: #070b12; box-sizing: border-box; place-items: center; }
.generation-player.has-video { display: grid; height: auto; min-height: 0; padding: 6px; }
.generation-player video { position: absolute; inset: 6px; display: block; width: calc(100% - 12px) !important; height: calc(100% - 12px) !important; max-width: calc(100% - 12px); max-height: calc(100% - 12px); object-fit: contain !important; object-position: center !important; background: #000; }
.player-placeholder { display: grid; gap: 10px; color: #94a3b8; text-align: center; place-items: center; }.player-placeholder span { display: grid; width: 64px; height: 64px; border-radius: 50%; background: rgb(255 255 255 / 10%); font-size: 24px; place-items: center; }.player-placeholder p { margin: 0; }
.generation-settings { display: grid; width: 100%; max-width: 100%; min-width: 0; overflow: hidden; gap: 0; padding: 20px; border-left: 1px solid var(--ant-color-border-secondary); background: var(--ant-color-bg-layout); box-sizing: border-box; }.setting-block { min-width: 0; padding: 14px; overflow: hidden; border: 1px solid var(--ant-color-border-secondary); border-radius: 0; background: var(--ant-color-bg-container); box-sizing: border-box; }.setting-block + .setting-block { border-top: 0; }.generation-settings > .setting-block:first-child { border-radius: 8px 8px 0 0; }.generation-settings > .setting-block:last-child { border-radius: 0 0 8px 8px; }.setting-block .section-heading { margin-bottom: 10px; }
.prompt-block :deep(textarea) { min-height: 240px !important; resize: vertical; }
.parameter-grid { display: grid; margin-bottom: 14px; gap: 10px; grid-template-columns: repeat(4, minmax(0, 1fr)); }.parameter-grid label { display: grid; min-width: 0; gap: 5px; }.parameter-grid label > span { color: var(--ant-color-text-secondary); font-size: 12px; }.parameter-grid :deep(.ant-select), .parameter-grid :deep(.ant-input-number) { width: 100%; }
.parameter-grid :deep(.ant-select-selector), .parameter-grid :deep(.ant-select-selection-item) { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.generate-actions { display: flex; align-items: center; justify-content: space-between; gap: 16px; }.generate-actions .audio-setting { display: flex; align-items: center; gap: 9px; color: var(--ant-color-text-secondary); font-size: 12px; }
.track-strip { display: flex; gap: 10px; overflow-x: auto; padding-bottom: 7px; }.track-strip button { position: relative; display: grid; width: 300px; flex: 0 0 300px; overflow: hidden; padding: 5px; border: 2px solid transparent; border-radius: 8px; color: var(--ant-color-text-secondary); background: var(--ant-color-fill-tertiary); cursor: pointer; gap: 5px; }.track-strip button.active { border-color: var(--ant-color-primary); }.track-frame-gallery { display: grid; width: 100%; height: 168px; gap: 3px; border-radius: 5px; background: var(--toon-ink); }.track-frame { position: relative; display: grid; min-width: 0; overflow: hidden; border-radius: 5px; background: var(--toon-ink); place-items: center; }.track-frame img { display: block; width: 100%; height: 100%; object-fit: contain; }.track-frame small { position: absolute; right: 4px; bottom: 4px; padding: 1px 4px; border-radius: 8px; color: #fff; font-size: 10px; background: rgb(0 0 0 / 65%); }.track-frame--empty { grid-column: 1 / -1; color: var(--ant-color-text-quaternary); font-size: 10px; }.track-strip > button > span:last-child { overflow: hidden; font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.generation-page > .history-section, .generation-page > .track-filmstrip { border-right: 0; border-left: 0; border-radius: 0; }
.generation-page > .history-section { border-bottom: 0; }
.batch-track-actions{display:flex;flex-wrap:wrap;justify-content:flex-end;gap:6px}.track-strip button .track-check{position:absolute;z-index:2;top:9px;left:9px;padding:4px;border-radius:5px;background:rgb(255 255 255 / 92%)}
.editor-clip-check :deep(.ant-checkbox-inner) { width: 16px; height: 16px; border: 2px solid var(--toon-muted); background: var(--toon-panel); }
.editor-clip-check:hover :deep(.ant-checkbox-inner), .editor-clip-check :deep(.ant-checkbox-input:focus + .ant-checkbox-inner) { border-color: var(--ant-color-primary); }
.editor-clip-check :deep(.ant-checkbox-checked .ant-checkbox-inner), .editor-clip-check :deep(.ant-checkbox-indeterminate .ant-checkbox-inner) { border-color: var(--ant-color-primary-text); background: var(--ant-color-primary-text); }
.editor-clip-check :deep(.ant-checkbox-checked .ant-checkbox-inner::after) { border-color: #fff; }
.asset-picker { display: grid; max-height: 65vh; overflow-y: auto; gap: 12px; grid-template-columns: repeat(auto-fill, minmax(130px, 1fr)); }.asset-picker button { position: relative; display: grid; overflow: hidden; padding: 6px; border: 1px solid var(--ant-color-border-secondary); border-radius: 8px; background: var(--ant-color-bg-container); cursor: pointer; gap: 6px; }.asset-picker button:hover { border-color: var(--ant-color-primary); }.asset-picker img { width: 100%; height: 112px; object-fit: contain; background: var(--ant-color-fill-secondary); }.asset-picker span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.asset-picker b { position: absolute; top: 10px; right: 10px; display: grid; width: 24px; height: 24px; border-radius: 50%; color: #fff; background: var(--ant-color-primary); place-items: center; }
@keyframes spin { to { transform: rotate(360deg); } }
@media (max-width: 900px) { .toonflow-workbench { grid-template-columns: 1fr; }.track-sidebar { display: flex; overflow-x: auto; border-right: 0; border-bottom: 1px solid var(--ant-color-border-secondary); }.sidebar-title { display: none; }.track-tab { width: 190px; flex: 0 0 190px; }.track-workspace { padding: 12px; } }
@media (max-width: 900px) { .toonflow-workbench-shell { grid-template-rows: 58px minmax(0, 1fr); }.workbench-topbar { grid-template-columns: 1fr auto; padding: 0 12px; }.workbench-tabs { grid-column: 1; justify-self: start; gap: 2px; overflow-x: auto; }.workbench-tabs button { min-width: 96px; padding: 0 10px; }.workbench-nav-status { grid-column: 2; font-size: 10px; } }
@media (max-width: 900px) { .generation-main { grid-template-columns: 1fr; }.generation-player { width: calc(100% - 32px); max-width: none; height: auto; min-height: 260px; aspect-ratio: 16 / 9; margin: 16px; }.generation-settings { padding: 16px; border-top: 1px solid var(--ant-color-border-secondary); border-left: 0; }.parameter-grid { grid-template-columns: 1fr; } }

.toonflow-workbench-shell,
.workbench-content,
.toonflow-workbench,
.track-workspace,
.editor-pane {
  min-width: 0;
  max-width: 100%;
  box-sizing: border-box;
}
.track-strip button { width: 300px; flex-basis: 300px; }
.track-strip img { height: 168px; }
.generation-page { min-height: 100%; grid-template-rows: minmax(0, 1fr) auto; }
.generation-main { min-height: 0; }
.generation-settings { overflow-y: auto; }
.track-filmstrip { align-self: end; }
.generation-player { grid-column: 1; grid-row: 1; }
.history-under-preview { grid-column: 1; grid-row: 2; margin: 0 20px 20px; }
.generation-settings { grid-column: 2; grid-row: 1 / span 2; }

/* Match the storyboard preview: one horizontal strip with per-track storyboard cards. */
.track-strip {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  overflow-x: auto;
  padding-bottom: 7px;
}

.track-strip-group {
  display: flex;
  width: max-content;
  min-width: 0;
  flex: 0 0 auto;
  flex-direction: column;
  gap: 4px;
  padding: 5px;
  border: 2px solid transparent;
  border-radius: 8px;
  background: var(--ant-color-fill-tertiary);
}

.track-strip-group.active {
  border-color: var(--ant-color-primary);
}

.track-strip-group-title {
  display: flex;
  height: 24px;
  align-items: center;
  gap: 8px;
  color: var(--ant-color-text-secondary);
  cursor: pointer;
  white-space: nowrap;
}

.track-strip-group-title .track-check {
  position: static;
  flex: 0 0 auto;
  padding: 2px;
  border-radius: 5px;
  background: rgb(255 255 255 / 92%);
}

.track-strip-group-title > span {
  overflow: hidden;
  max-width: 360px;
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.track-strip-group-items {
  display: flex;
  width: max-content;
  height: 168px;
  gap: 3px;
}

.track-strip .track-shot {
  position: relative;
  display: grid;
  width: 150px;
  height: 168px;
  flex: 0 0 150px;
  overflow: hidden;
  padding: 0;
  border: 0;
  border-radius: 5px;
  background: var(--toon-ink);
  cursor: pointer;
  place-items: center;
}

.track-strip .track-shot img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: contain;
}

.track-shot small {
  position: absolute;
  right: 4px;
  bottom: 4px;
  max-width: calc(100% - 8px);
  overflow: hidden;
  padding: 1px 4px;
  border-radius: 8px;
  color: #fff;
  font-size: 10px;
  text-overflow: ellipsis;
  white-space: nowrap;
  background: rgb(0 0 0 / 65%);
}

.track-shot--empty {
  color: var(--ant-color-text-quaternary);
  font-size: 11px;
}
.history-under-preview .video-grid { grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); gap: 10px; }
.editor-timeline { min-width: 0; overflow: hidden; }
.timeline-lane { display: grid; min-width: 0; align-items: start; grid-template-columns: 72px minmax(0, 1fr); }
.timeline-lane-label { display: grid; min-height: 74px; align-items: center; padding: 6px 10px; color: var(--ant-color-text-secondary); font-size: 12px; background: var(--ant-color-fill-tertiary); }
.timeline-scroll { min-width: 0; overflow-x: auto; overflow-y: hidden; padding-bottom: 4px; }
.timeline-scroll .timeline-ruler { min-width: 560px; padding-left: 0; box-sizing: border-box; }
.timeline-scroll .clip-track { min-width: 560px; box-sizing: border-box; }
.timeline-scroll .clip-track { padding-left: 6px; }
.timeline-scroll .empty-track { margin-left: 0; }
.timeline-scroll .timeline-clip { flex-grow: 0; flex-shrink: 0; }
.timeline-scroll .timeline-clip > video { pointer-events: none; }

/* Second-pass generation workspace polish. */
.generation-page {
  gap: 12px;
  padding: 12px;
  background: var(--ant-color-bg-layout);
}

.generation-main {
  gap: 12px;
  overflow: visible;
}

.generation-player {
  width: calc(100% - 24px);
  min-height: 260px;
  margin: 12px;
  border: 1px solid var(--ant-color-border-secondary);
  border-radius: 14px;
  box-shadow: 0 10px 24px rgb(15 23 42 / 8%);
}

.player-placeholder {
  min-height: 220px;
  padding: 24px;
  border: 1px dashed rgb(148 163 184 / 45%);
  border-radius: 12px;
  background: radial-gradient(circle at 50% 35%, rgb(30 41 59 / 72%), rgb(7 11 18 / 96%));
  box-sizing: border-box;
}

.player-placeholder span {
  box-shadow: 0 0 0 8px rgb(255 255 255 / 4%);
}

.generation-settings {
  gap: 10px;
  padding: 12px;
  border: 1px solid var(--ant-color-border-secondary);
  border-radius: 14px;
  background: var(--ant-color-bg-container);
  box-shadow: 0 6px 18px rgb(15 23 42 / 6%);
}

.generation-settings > .setting-block,
.generation-settings > .setting-block:first-child,
.generation-settings > .setting-block:last-child {
  border: 1px solid var(--ant-color-border-secondary);
  border-radius: 10px;
}

.generation-settings > .setting-block + .setting-block {
  border-top: 1px solid var(--ant-color-border-secondary);
}

.prompt-block :deep(textarea) {
  min-height: 190px !important;
  padding: 10px 12px;
  border-radius: 8px;
  line-height: 1.6;
}

.generate-actions {
  margin-top: 4px;
  padding-top: 12px;
  border-top: 1px dashed var(--ant-color-border-secondary);
}

.generate-actions > .ant-btn {
  min-width: 112px;
  font-weight: 600;
}

.generation-page > .history-section,
.generation-page > .track-filmstrip {
  margin: 0;
  border: 1px solid var(--ant-color-border-secondary);
  border-radius: 12px;
  background: var(--ant-color-bg-container);
  box-shadow: 0 6px 16px rgb(15 23 42 / 5%);
}

.generation-page > .history-section {
  padding: 14px;
}

.generation-page > .track-filmstrip {
  padding: 14px 14px 10px;
}

.generation-page > .track-filmstrip .section-heading {
  margin-bottom: 10px;
}

.track-strip-group {
  transition: border-color 160ms ease, box-shadow 160ms ease, transform 160ms ease;
}

.track-strip-group:hover {
  border-color: var(--ant-color-primary-border);
  box-shadow: 0 5px 14px rgb(15 23 42 / 8%);
  transform: translateY(-1px);
}

.track-strip-group.active {
  box-shadow: 0 0 0 2px var(--ant-color-primary-bg), 0 5px 14px rgb(22 119 255 / 12%);
}

.video-card {
  transition: border-color 160ms ease, box-shadow 160ms ease, transform 160ms ease;
}

.video-card:hover {
  border-color: var(--ant-color-primary-border);
  box-shadow: 0 6px 16px rgb(15 23 42 / 9%);
  transform: translateY(-1px);
}

.video-card--selected {
  box-shadow: 0 0 0 2px var(--ant-color-primary-bg), 0 6px 16px rgb(22 119 255 / 12%);
}

.video-placeholder {
  min-height: 96px;
  background: radial-gradient(circle at 50% 35%, #273449, var(--toon-ink));
}

.editor-workspace {
  box-shadow: 0 8px 20px rgb(15 23 42 / 6%);
}

.editor-stage {
  background: radial-gradient(circle at 50% 30%, var(--toon-panel), var(--ant-color-bg-layout));
}

@media (max-width: 900px) {
  .generation-page {
    gap: 8px;
    padding: 8px;
  }

  .generation-main {
    gap: 8px;
  }

  .generation-player {
    width: calc(100% - 16px);
    margin: 8px;
  }

  .generation-settings {
    gap: 8px;
    padding: 8px;
  }

  .generation-page > .history-section,
  .generation-page > .track-filmstrip {
    border-radius: 10px;
  }
}

/* Make generation parameters readable in the narrow settings column. */
.parameter-block {
  padding: 16px;
}

.parameter-heading {
  margin-bottom: 14px;
}

.parameter-heading > div {
  min-width: 0;
}

.parameter-heading > div > span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.parameter-heading :deep(.ant-tag) {
  flex: none;
  margin: 0;
  font-weight: 600;
}

.parameter-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
  margin-bottom: 16px;
}

.parameter-grid label {
  gap: 7px;
  padding: 9px;
  border: 1px solid var(--ant-color-border-secondary);
  border-radius: 8px;
  background: var(--ant-color-bg-layout);
}

.parameter-grid label > span:first-child {
  color: var(--ant-color-text-secondary);
  font-size: 11px;
  font-weight: 600;
}

.parameter-grid :deep(.ant-select-selector),
.parameter-grid :deep(.ant-input-number) {
  border-radius: 6px;
}

.duration-readonly {
  display: flex;
  min-height: 24px;
  align-items: center;
  padding: 3px 8px;
  overflow: hidden;
  border-radius: 6px;
  color: var(--ant-color-text-secondary);
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
  background: var(--ant-color-fill-tertiary);
}

.frame-source-note {
  display: grid;
  grid-column: 1 / -1;
  gap: 4px;
  padding: 9px 11px;
  border: 1px solid var(--ant-color-primary-border);
  border-radius: 8px;
  color: var(--ant-color-text-secondary);
  font-size: 11px;
  line-height: 1.5;
  background: var(--ant-color-primary-bg);
}

.frame-source-note--cross-scene {
  border-color: var(--ant-color-warning-border);
  background: var(--ant-color-warning-bg);
}

.frame-source-warning {
  color: var(--ant-color-warning-text);
  font-weight: 600;
}

.transition-setting-meta {
  display: flex;
  grid-column: 1 / -1;
  flex-wrap: wrap;
  justify-content: space-between;
  gap: 6px 14px;
  color: var(--ant-color-text-tertiary);
  font-size: 11px;
}

@media (max-width: 900px) {
  .parameter-grid {
    grid-template-columns: 1fr;
  }
}

/* Refine the three editing surfaces without changing the playback model. */
.editor-pane {
  padding: 16px 22px 22px;
}

.editor-workspace {
  min-height: 520px;
  border-radius: 12px 12px 0 0;
  box-shadow: 0 8px 20px rgb(15 23 42 / 6%);
  grid-template-columns: 220px minmax(0, 1fr) 248px;
}

.editor-media-library,
.editor-properties {
  padding: 14px;
}

.editor-media-library header,
.editor-properties header {
  height: auto;
  min-height: 38px;
  align-items: flex-start;
  margin-bottom: 12px;
}

.editor-panel-title {
  display: grid;
  min-width: 0;
  gap: 3px;
}

.editor-panel-title small {
  color: var(--ant-color-text-tertiary);
  font-size: 11px;
}

.editor-media-header-actions {
  align-items: flex-start;
}

.editor-media-header-actions :deep(.ant-tag) {
  margin: 0;
  line-height: 20px;
}

.editor-media-library > button {
  margin-bottom: 8px;
  padding: 6px;
  border-radius: 8px;
  transition: border-color 160ms ease, background 160ms ease, box-shadow 160ms ease, transform 160ms ease;
}

.editor-media-library > button:hover {
  border-color: var(--ant-color-primary-border);
  box-shadow: 0 4px 10px rgb(15 23 42 / 7%);
  transform: translateY(-1px);
}

.editor-media-library > button.active {
  box-shadow: 0 0 0 2px var(--ant-color-primary-bg), 0 4px 10px rgb(22 119 255 / 9%);
}

.editor-stage {
  min-width: 0;
  padding: 14px 18px 12px;
  background: radial-gradient(circle at 50% 20%, var(--toon-panel), var(--ant-color-bg-layout));
  grid-template-rows: 32px minmax(0, 1fr) 48px;
}

.editor-stage-heading {
  display: flex;
  min-width: 0;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}

.editor-stage-heading > div {
  display: flex;
  min-width: 0;
  align-items: baseline;
  gap: 8px;
}

.editor-stage-heading span {
  overflow: hidden;
  color: var(--ant-color-text-tertiary);
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.editor-stage-heading :deep(.ant-tag) {
  margin: 0;
  flex: none;
}

.editor-canvas {
  border: 1px solid rgb(15 23 42 / 12%);
  border-radius: 10px;
  box-shadow: 0 8px 18px rgb(15 23 42 / 12%);
}

.editor-transport {
  height: 42px;
  gap: 8px;
}

.editor-transport :deep(.ant-btn) {
  box-shadow: none;
}

.editor-transport-status {
  display: grid;
  min-width: 56px;
  gap: 2px;
  margin-left: 4px;
  text-align: center;
}

.editor-transport-status b {
  color: var(--ant-color-text-secondary);
  font-size: 11px;
  font-weight: 600;
}

.editor-transport-status small {
  color: var(--ant-color-text-tertiary);
  font-size: 10px;
}

.editor-properties {
  background: linear-gradient(180deg, var(--ant-color-bg-container), var(--ant-color-bg-layout));
}

.editor-properties label {
  padding: 11px 0;
}

.editor-properties label b {
  color: var(--ant-color-text);
  font-size: 12px;
}

.volume-control {
  margin: 16px 0;
  padding: 10px;
  border-radius: 8px;
  background: var(--ant-color-fill-tertiary);
}

.editor-timeline {
  margin-top: 12px;
  border: 1px solid var(--ant-color-border-secondary);
  border-radius: 0 0 12px 12px;
  box-shadow: 0 6px 16px rgb(15 23 42 / 5%);
}

.timeline-toolbar {
  min-height: 26px;
}

.timeline-clip {
  transition: border-color 160ms ease, box-shadow 160ms ease, transform 160ms ease;
}

.timeline-clip:hover {
  box-shadow: 0 4px 10px rgb(15 23 42 / 16%);
  transform: translateY(-1px);
}

.generation-scene-strips { display: flex; min-width: 0; flex-direction: column; gap: 10px; }
.generation-scene-strip { min-width: 0; overflow: hidden; padding: 9px; border: 1px solid var(--ant-color-border-secondary); border-radius: 9px; background: var(--ant-color-bg-layout); }
.generation-scene-heading { display: flex; min-width: 0; align-items: baseline; gap: 10px; margin-bottom: 7px; }
.generation-scene-heading b { overflow: hidden; color: var(--ant-color-text); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.generation-scene-heading span { flex: none; color: var(--ant-color-text-tertiary); font-size: 10px; }
.editor-scene-library { min-width: 0; margin-bottom: 10px; padding: 6px; border: 1px solid var(--ant-color-border-secondary); border-radius: 9px; background: var(--ant-color-bg-layout); }
.editor-media-library .editor-scene-library > header { display: flex; width: 100%; min-width: 0; min-height: 28px; align-items: center; gap: 6px; justify-content: space-between; margin: 0 0 6px; }
.editor-scene-library > header :deep(.ant-checkbox-wrapper) { min-width: 0; overflow: hidden; font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.editor-scene-library > header > small { flex: none; color: var(--ant-color-text-tertiary); font-size: 10px; }
.editor-media-library .editor-scene-library > button { position: relative; display: grid; width: 100%; min-width: 0; align-items: center; gap: 8px; margin-bottom: 6px; padding: 6px; border: 1px solid transparent; border-radius: 8px; text-align: left; background: var(--ant-color-fill-tertiary); cursor: pointer; grid-template-columns: 68px minmax(0, 1fr); transition: border-color 160ms ease, background 160ms ease, box-shadow 160ms ease, transform 160ms ease; }
.editor-media-library .editor-scene-library > button:last-child { margin-bottom: 0; }
.editor-media-library .editor-scene-library > button:hover { border-color: var(--ant-color-primary-border); box-shadow: 0 4px 10px rgb(15 23 42 / 7%); transform: translateY(-1px); }
.editor-media-library .editor-scene-library > button.active { border-color: var(--ant-color-primary); background: var(--ant-color-primary-bg); box-shadow: 0 0 0 2px var(--ant-color-primary-bg), 0 4px 10px rgb(22 119 255 / 9%); }
.timeline-scene-track { min-height: 102px; align-items: stretch; gap: 6px; }
.timeline-scene-group { display: flex; min-width: 0; flex-direction: column; gap: 4px; padding: 4px; border: 1px solid color-mix(in srgb, var(--ant-color-primary) 25%, var(--ant-color-border-secondary)); border-radius: 6px; background: var(--ant-color-bg-container); }
.timeline-scene-group > header { overflow: hidden; color: var(--ant-color-primary); font-size: 10px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.timeline-scene-clips { display: flex; min-height: 70px; gap: 2px; }
.timeline-scene-clips .timeline-clip { height: 70px; }
.editor-timeline .timeline-lane-label { min-height: 102px; }

@media (max-width: 1200px) {
  .editor-workspace {
    grid-template-columns: 190px minmax(0, 1fr) 214px;
  }

  .editor-pane {
    padding-inline: 14px;
  }
}

@media (max-width: 900px) {
  .editor-pane {
    padding: 12px;
  }

  .editor-workspace {
    min-height: 0;
    grid-template-columns: 1fr;
  }

  .editor-media-library {
    max-height: 238px;
    border-right: 0;
    border-bottom: 1px solid var(--ant-color-border-secondary);
  }

  .editor-stage {
    min-height: 390px;
    padding: 12px;
    grid-template-rows: 30px minmax(0, 1fr) 48px;
  }

  .editor-properties {
    border-top: 1px solid var(--ant-color-border-secondary);
    border-left: 0;
  }

  .editor-timeline {
    margin-top: 8px;
    border-radius: 0 0 10px 10px;
  }
}

/* Keep the generation cards in their own columns. The right settings card
 * must not stretch the preview/history column to the same height. */
.generation-page {
  display: block;
  min-height: 0;
  padding: 16px;
  overflow: visible;
  background: var(--ant-color-bg-layout);
}

.generation-main {
  display: grid;
  min-height: 0;
  align-items: start;
  gap: 16px;
  overflow: visible;
  grid-template-columns: minmax(0, 1.12fr) minmax(360px, 0.88fr);
}

.generation-left-column {
  display: grid;
  min-width: 0;
  align-content: start;
  gap: 16px;
}

.generation-player {
  width: 100%;
  min-height: 0;
  margin: 0;
  border-radius: 14px;
}

.history-under-preview {
  grid-column: auto;
  grid-row: auto;
  margin: 0;
}

.generation-settings {
  grid-column: auto;
  grid-row: auto;
  align-self: start;
  max-height: calc(100vh - 142px);
  overflow-y: auto;
}

.generation-page > .track-filmstrip {
  margin-top: 16px;
  align-self: auto;
}

@media (max-width: 1100px) {
  .generation-main {
    grid-template-columns: minmax(0, 1fr) minmax(320px, 0.82fr);
  }
}

@media (max-width: 900px) {
  .generation-page {
    padding: 10px;
  }

  .generation-main {
    grid-template-columns: 1fr;
  }

  .generation-settings {
    max-height: none;
    overflow: visible;
  }

  .generation-page > .track-filmstrip {
    margin-top: 10px;
  }
}
</style>
