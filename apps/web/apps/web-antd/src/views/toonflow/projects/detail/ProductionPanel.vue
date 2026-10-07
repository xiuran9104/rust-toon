<script setup lang="ts">
/* eslint-disable vue/no-mutating-props -- panel context is an intentionally shared reactive view model */
import type { ToonflowApi } from '#/api/toonflow';

import { computed, defineAsyncComponent, reactive, ref, watch } from 'vue';

import { Button, Card, Col, Form, Input, InputNumber, message, Modal, Row, Select, Space, Tag } from 'ant-design-vue';

import { addStoryboard, editStoryboardInfo, getAgentRunEvents, getSceneConsistencyCatalog } from '#/api/toonflow';

import AgentRunEventDrawer from '../../components/AgentRunEventDrawer.vue';
import { normalizeProductionDocument } from '../production-flow-document';
import ProductionFlowCanvas from '../ProductionFlowCanvas.vue';
import { defaultSceneStateId, isStateValidForScene, sceneConsistencyIssues, sceneStateOptions, sceneStateTimelineViolation } from '../scene-consistency';
import { buildPlannedScenes } from '../scene-consistency-planning';
import SceneConsistencyManager from '../SceneConsistencyManager.vue';

const props = defineProps<{ context: any }>();

const AgentChat = defineAsyncComponent(() => import('../AgentChat.vue'));
const ImageFlowEditor = defineAsyncComponent(() => import('../ImageFlowEditor.vue'));

const storyboardModalOpen = ref(false);
const insertAfterStoryboardId = ref<number>();
const agentEventsOpen = ref(false); const agentEventRunId = ref<number>(); const agentEvents = ref<any[]>([]); const agentEventsLoading = ref(false);
const sceneConsistencyOpen = ref(false);
const sceneCatalog = ref<ToonflowApi.SceneConsistencyCatalog>({ scenes: [] });
let sceneCatalogRequestVersion = 0;
const storyboardForm = reactive({
  associateAssetsIds: [] as number[],
  cameraMove: undefined as string | undefined,
  duration: 4,
  id: undefined as number | undefined,
  prompt: '',
  sceneKey: '',
  sceneStateId: undefined as number | undefined,
  shouldGenerateImage: 1,
  shotSize: undefined as string | undefined,
  state: '未生成',
  track: 'main',
  videoDesc: '',
});

const SHOT_SIZE_OPTIONS = ['特写', '近景', '中景', '全景', '远景', '大远景'].map(
  (value) => ({ value }),
);
const CAMERA_MOVE_OPTIONS = [
  '固定',
  '推镜',
  '拉镜',
  '摇镜',
  '移镜',
  '跟镜',
  '升降',
  '环绕',
].map((value) => ({ value }));

const storyboardSceneStateOptions = computed(() =>
  sceneStateOptions(sceneCatalog.value, storyboardForm.sceneKey),
);
const sceneIssues = computed(() =>
  sceneConsistencyIssues(sceneCatalog.value, props.context.storyboards ?? []),
);
const sceneIssueLabel = computed(() => {
  const parts = [];
  if (sceneIssues.value.missingMasters) parts.push(`${sceneIssues.value.missingMasters} 场缺母版`);
  if (sceneIssues.value.unboundStoryboards) parts.push(`${sceneIssues.value.unboundStoryboards} 镜未绑定`);
  if (sceneIssues.value.staleStoryboards) parts.push(`${sceneIssues.value.staleStoryboards} 镜需重生`);
  return parts.length ? `场景一致性 · ${parts.join(' / ')}` : '场景一致性 · 已就绪';
});
const plannedScenes = computed(() => {
  if (
    !props.context.selectedScriptId ||
    props.context.loadedFlowProjectId !== props.context.projectId ||
    props.context.loadedFlowScriptId !== props.context.selectedScriptId
  ) {
    return [];
  }
  let flow: Record<string, unknown>;
  try {
    flow = JSON.parse(props.context.flowText || '{}');
  } catch {
    return [];
  }
  const scriptPlan = normalizeProductionDocument(
    flow.scriptPlan || flow.directorPlan,
    'scriptPlan',
  );
  const storyboardTable = normalizeProductionDocument(
    flow.storyboardTable,
    'storyboardTable',
  );
  return buildPlannedScenes(
    scriptPlan,
    storyboardTable,
    props.context.productionAssets ?? [],
  );
});

async function refreshSceneCatalog() {
  const targetProjectId = props.context.projectId;
  const targetScriptId = props.context.selectedScriptId;
  const requestVersion = ++sceneCatalogRequestVersion;
  if (!targetScriptId) {
    sceneCatalog.value = { scenes: [] };
    return;
  }
  const nextCatalog = await getSceneConsistencyCatalog(
    targetProjectId,
    targetScriptId,
  );
  if (
    requestVersion !== sceneCatalogRequestVersion ||
    props.context.projectId !== targetProjectId ||
    props.context.selectedScriptId !== targetScriptId
  ) return;
  sceneCatalog.value = nextCatalog;
}

watch(
  () => [props.context.projectId, props.context.selectedScriptId],
  () => void refreshSceneCatalog(),
  { immediate: true },
);

function openStoryboard(item?: ToonflowApi.Storyboard, keepInsertPosition = false) {
  props.context.focusProductionNode?.('storyboard');
  if (!keepInsertPosition) insertAfterStoryboardId.value = undefined;
  Object.assign(storyboardForm, {
    associateAssetsIds: [...(item?.associateAssetsIds ?? [])],
    cameraMove: item?.cameraMove,
    duration: item?.duration ?? 4,
    id: item?.id,
    prompt: item?.prompt ?? '',
    sceneKey: item
      ? (item.sceneKey ?? '')
      : (props.context.storyboards.at(-1)?.sceneKey ??
        plannedScenes.value[0]?.sceneKey ??
        ''),
    sceneStateId: item?.sceneStateId ?? props.context.storyboards.at(-1)?.sceneStateId,
    shouldGenerateImage: item?.shouldGenerateImage ?? 1,
    shotSize: item?.shotSize,
    state: item?.state ?? '未生成',
    track: item?.track ?? 'main',
    videoDesc: item?.videoDesc ?? '',
  });
  storyboardModalOpen.value = true;
}
async function openAgentRun(runId: number) { agentEventRunId.value = runId; agentEventsOpen.value = true; agentEventsLoading.value = true; try { agentEvents.value = await getAgentRunEvents(runId); } finally { agentEventsLoading.value = false; } }

function beginInsertStoryboard(item: ToonflowApi.Storyboard) {
  insertAfterStoryboardId.value = item.id;
  openStoryboard(undefined, true);
  storyboardForm.sceneKey = item.sceneKey ?? '';
  storyboardForm.sceneStateId = item.sceneStateId;
}

watch(
  () => storyboardForm.sceneKey,
  (sceneKey) => {
    if (!storyboardModalOpen.value) return;
    if (!isStateValidForScene(sceneCatalog.value, sceneKey, storyboardForm.sceneStateId)) {
      storyboardForm.sceneStateId = defaultSceneStateId(sceneCatalog.value, sceneKey);
    }
  },
);

async function saveStoryboardForm() {
  if (!props.context.selectedScriptId) return message.warning('请先选择剧本');
  const sceneKey = storyboardForm.sceneKey.trim().toLowerCase();
  if (!/^sc[1-9]\d*$/.test(sceneKey)) {
    return message.warning('场次键请使用 scN 格式，例如 sc1、sc2');
  }
  storyboardForm.sceneKey = sceneKey;
  if (!storyboardForm.sceneStateId) {
    storyboardForm.sceneStateId = defaultSceneStateId(sceneCatalog.value, sceneKey);
  }
  const timelineViolation = sceneStateTimelineViolation(
    sceneCatalog.value,
    props.context.storyboards ?? [],
    {
      id: storyboardForm.id,
      sceneKey,
      sceneStateId: storyboardForm.sceneStateId,
    },
    insertAfterStoryboardId.value,
  );
  if (timelineViolation) {
    return message.warning(
      `${timelineViolation.sceneKey.toUpperCase()} 状态不能从“${timelineViolation.earlierStateName}”回到“${timelineViolation.laterStateName}”。如剧情发生修复，请新建一个基于前态的后续状态，不能重新选择 base。`,
    );
  }
  if (storyboardForm.id) {
    await editStoryboardInfo({ ...storyboardForm, id: storyboardForm.id });
  } else {
    const created = await addStoryboard({
      ...storyboardForm,
      projectId: props.context.projectId,
      scriptId: props.context.selectedScriptId,
    });
    const afterId = insertAfterStoryboardId.value;
    await props.context.reloadFlow();
    if (afterId && created.id) {
      const ids = props.context.storyboards.map((item: ToonflowApi.Storyboard) => item.id).filter((id: number) => id !== created.id);
      const index = ids.indexOf(afterId);
      ids.splice(index < 0 ? ids.length : index + 1, 0, created.id);
      await props.context.saveStoryboardOrder(ids);
    }
    insertAfterStoryboardId.value = undefined;
  }
  storyboardModalOpen.value = false;
  if (storyboardForm.id) await props.context.reloadFlow();
  await refreshSceneCatalog();
}

async function onSceneCatalogChanged(catalog: ToonflowApi.SceneConsistencyCatalog) {
  sceneCatalogRequestVersion += 1;
  sceneCatalog.value = catalog;
  await props.context.reloadFlow();
}
</script>
<template><section class="detail-panel detail-panel--production"><Row :gutter="16" class="production-layout"><Col :span="24"><Space class="production-tools mb-3" wrap><Select :value="context.selectedScriptId" :options="context.scriptOptions" placeholder="选择剧本" style="width:240px" @change="(value: unknown) => context.changeProductionScript(value)" /><Button @click="context.loadFlow">刷新制作数据</Button><Button type="primary" @click="context.saveFlowText">保存分镜工作区</Button><Button :loading="context.rebuildingStoryboardPanel" @click="context.rebuildStoryboardPanel">{{ context.storyboards.length ? 'Agent 修复分镜面板' : 'Agent 生成分镜面板' }}</Button><Button :danger="sceneIssues.missingMasters > 0 || sceneIssues.unboundStoryboards > 0" @click="sceneConsistencyOpen = true">{{ sceneIssueLabel }}</Button><Button @click="openStoryboard()">新增分镜</Button><Button @click="context.previewAllStoryboardImages">合成预览</Button><Button v-if="context.productionAgentCollapsed" @click="context.productionAgentCollapsed = false">展开 Agent</Button></Space><ProductionFlowCanvas :key="`${context.projectId}:${context.selectedScriptId}:${context.loadedFlowScriptId ?? 'loading'}`" :selected-node-id="context.productionNodeId" @update:selected-node-id="context.rememberProductionNode" :ref="context.setProductionFlowCanvasRef" :assets="context.productionAssets" :flow-text="context.flowText" :image-model="context.project?.imageModel" :image-quality="context.imageQuality" :script="context.selectedScript" :storyboard-busy="context.storyboardBusy" :storyboard-progress-current="context.storyboardProgressCurrent" :storyboard-progress-total="context.storyboardProgressTotal" :storyboard-run-state="context.storyboardNodeRunState" :storyboards="context.storyboards" :video-mode="context.videoMode" :video-model="context.project?.videoModel" :video-ratio="context.project?.videoRatio" :video-tracks="context.videoTracks" :workflow-node-runs="context.workflowNodeRuns" @cancel-node="context.cancelProductionWorkflowNode" @cancel-track-video="context.cancelVideo" @cancel-storyboards="context.cancelStoryboardWorkflow" @delete-track-video="context.removeVideo" @batch-delete-storyboards="context.batchDeleteSelectedStoryboards" @batch-generate-video-prompts="context.generateSelectedVideoPrompts" @batch-generate-videos="context.generateSelectedVideos" @batch-download-videos="context.downloadSelectedVideos" @refresh-workbench="context.loadFlow" @edit-asset="context.openAssetImageFlow" @edit-storyboard="openStoryboard" @edit-storyboard-image="context.openStoryboardImageFlow" @export-storyboard-images="context.downloadSelectedStoryboardImages" @export-video="context.exportVideo" @generate-storyboards="context.generateStoryboards" @generate-derived-asset="context.generateDerivedAsset" @generate-track-video="context.generateVideo" @generate-video-prompt="context.createVideoPrompt" @insert-storyboard-after="beginInsertStoryboard" @open-video-track="context.openVideoTrack" @open-agent-run="openAgentRun" @retry-track-video="context.retryVideo" @retry-storyboards="context.retryStoryboardWorkflow" @retry-node="context.retryProductionWorkflowNode" @run-node="context.runProductionWorkflowNode" @run-sequence="context.runProductionWorkflowSequence" @remove-storyboard="context.deleteStoryboard" @reorder-storyboards="context.saveStoryboardOrder" @save-positions="context.saveProductionCanvasPositions" @save-workflow="context.saveProductionWorkflow" @save-video-prompt="context.saveTrackPrompt" @select-track-video="context.chooseVideo" @update-flow-section="context.updateProductionFlowSection" /></Col><Col v-if="!context.productionAgentCollapsed" :span="24" class="production-agent-panel"><Card class="production-agent-card" size="small" title="分镜制作 Agent"><template #extra><Space><Tag color="blue">{{ context.productionAgentActivity }}</Tag><Button size="small" @click="context.productionAgentCollapsed = true">折叠</Button><Button size="small" @click="context.resetProductionAgent">重新开始</Button></Space></template><div class="production-agent-context"><Tag color="green">读取剧本与基础资产</Tag></div><AgentChat :ref="context.setProductionAgentChatRef" agent-type="productionAgent" :project-id="context.projectId" :script-id="context.selectedScriptId" :messages="context.productionChatMessages" starter-label="开始制作视频" starter-prompt="立即读取当前已有剧本和人物基础资产，从「人物衍生资产分析」开始制作。人物父资产都是白色基础内衣底模，必须逐一检查当前剧本中的每个角色，并为每个出场角色写入至少一套符合其身份和剧情的正式服装衍生；另行补充剧本明确出现的换装、重伤、变身或稳定形态变化。请直接调用 run_sub_agent_derive_assets 实际写入，不要重新生成剧本，不要只汇报状态，也不要在执行前询问是否开始。完成后展示衍生清单并暂停等待我确认生成。" @activity="context.onProductionAgentActivity" @clear-memory="context.clearProductionAgentMemory" @tool-result="context.onAgentToolResult" @workspace-preview="context.previewProductionFlowSection" /></Card></Col></Row><Modal root-class-name="toon-overlay" v-model:open="context.trackBindingOpen" :title="`调整轨道 ${context.trackBindingTarget?.id ?? ''} 的分镜`" width="720px" @ok="context.confirmTrackBinding"><p class="mb-3 text-gray-500">选中的分镜会从原轨道移入当前轨道，可多选。</p><Select v-model:value="context.trackBindingStoryboardIds" mode="multiple" option-filter-prop="label" :options="context.storyboards.map((item:any)=>({value:item.id,label:`${item.index ?? '-'} · ${item.videoDesc || item.prompt || `分镜 ${item.id}`}`}))" placeholder="选择要移入该轨道的分镜" style="width:100%" /></Modal></section><SceneConsistencyManager v-model:open="sceneConsistencyOpen" :assets="context.productionAssets" :planned-scenes="plannedScenes" :project-id="context.projectId" :script-id="context.selectedScriptId" @changed="onSceneCatalogChanged" /><AgentRunEventDrawer :open="agentEventsOpen" :run-id="agentEventRunId" :events="agentEvents" :loading="agentEventsLoading" @close="agentEventsOpen = false" @refresh="openAgentRun(agentEventRunId!)" /><Modal root-class-name="toon-overlay" v-model:open="storyboardModalOpen" :title="storyboardForm.id ? '编辑分镜' : '新增分镜'" width="820px" @ok="saveStoryboardForm"><Form :label-col="{span:4}"><Form.Item label="场次键"><Input v-model:value="storyboardForm.sceneKey" placeholder="例如 sc2（同一空间保持一致）" /></Form.Item><Form.Item label="场景状态"><Select v-model:value="storyboardForm.sceneStateId" :options="storyboardSceneStateOptions" placeholder="选择该场次当前的空间状态" /><div class="mt-1 text-xs text-gray-500">同场景更换机位或人物动作不换状态；门、桌等发生永久破坏后切换到新状态。</div></Form.Item><Form.Item label="轨道"><Input v-model:value="storyboardForm.track" /></Form.Item><Form.Item label="时长"><InputNumber v-model:value="storyboardForm.duration" :min="1" /></Form.Item><Form.Item label="景别"><Select v-model:value="storyboardForm.shotSize" :options="SHOT_SIZE_OPTIONS" allow-clear placeholder="如特写、全景；特写/近景只能绑定一个人物主体" /></Form.Item><Form.Item label="运镜"><Select v-model:value="storyboardForm.cameraMove" :options="CAMERA_MOVE_OPTIONS" allow-clear placeholder="如推镜、跟镜" /></Form.Item><Form.Item label="关联资产"><Select v-model:value="storyboardForm.associateAssetsIds" :options="context.assetOptions" mode="multiple" /></Form.Item><Form.Item label="画面描述"><Input.TextArea v-model:value="storyboardForm.videoDesc" :rows="4" /></Form.Item><Form.Item label="图片提示词"><Input.TextArea v-model:value="storyboardForm.prompt" :rows="5" /></Form.Item></Form></Modal><Modal root-class-name="toon-overlay" v-model:open="context.flowImageModalOpen" :title="`${context.editingAssetName} · 图片生成流`" width="94vw" :footer="null"><ImageFlowEditor :key="context.imageFlowEditorKey" :asset-options="context.imageFlowAssetOptions" v-model:edges="context.imageFlowEdges" v-model:nodes="context.imageFlowNodes" :loading-node-id="context.generatingImageNodeId" @add="context.addImageFlowNode" @agent="context.planImageEditWithAgent" @connect="context.connectImageFlow" @generate="context.createFlowImage" @remove="context.removeImageFlowNode" @save="context.saveVisualImageFlow" @select-asset="context.selectImageFlowAsset" @upload="context.uploadImageFlowReference" /></Modal><Modal root-class-name="toon-overlay" v-model:open="context.storyboardPreviewOpen" title="分镜合成预览" width="1100px"><img v-if="context.storyboardPreview" :src="context.storyboardPreview" class="w-full rounded border" /><template #footer><Button @click="context.storyboardPreviewOpen = false">关闭</Button><Button type="primary" @click="context.downloadAllStoryboardImages">下载 PNG</Button></template></Modal></template>
