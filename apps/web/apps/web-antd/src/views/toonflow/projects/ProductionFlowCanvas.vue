<script lang="ts" setup>
import type { Connection, Edge, Node } from '@vue-flow/core';
import type { ToonflowApi, WorkflowNodeRun } from '#/api/toonflow';
import type { ProductionWorkflowDefinition } from './production-workflow';

import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';

import { Background } from '@vue-flow/background';
import { Controls } from '@vue-flow/controls';
import { MiniMap } from '@vue-flow/minimap';
import { Handle, Position, VueFlow } from '@vue-flow/core';
import {
  Button,
  Descriptions,
  Drawer,
  Empty,
  Form,
  Input,
  InputNumber,
  message,
  Modal,
  Select,
  Space,
  Switch,
  Tag,
  Typography,
} from 'ant-design-vue';

import { validateWorkflow } from '#/api/toonflow';
import { MarkdownView } from '#/components/markdown-view';
import { assetFileUrl } from '../assets/asset-types';

import '@vue-flow/core/dist/style.css';
import '@vue-flow/core/dist/theme-default.css';
import '@vue-flow/controls/dist/style.css';

import ProductionAssetStrip from './ProductionAssetStrip.vue';
import { normalizeProductionDocument } from './production-flow-document';
import {
  productionNodeRuntime,
  selectedWorkbenchCover,
} from './production-flow-runtime';
import {
  defaultProductionWorkflow,
  normalizeProductionWorkflow,
  workflowHasCycle,
  workflowPath,
  workflowNodeMeta,
  workflowNodeViewType,
} from './production-workflow';
import StoryboardPanel from './StoryboardPanel.vue';

const scriptExpanded = ref(false);

const props = defineProps<{
  selectedNodeId?: string;
  assets: ToonflowApi.Asset[];
  storyboardBusy?: boolean;
  storyboardProgressCurrent?: number;
  storyboardProgressTotal?: number;
  storyboardRunState?: string;
  flowText: string;
  imageModel?: number;
  imageQuality?: string;
  script?: ToonflowApi.Script;
  storyboards: ToonflowApi.Storyboard[];
  videoMode?: string;
  videoModel?: number;
  videoRatio?: string;
  videoTracks: any[];
  workflowNodeRuns?: Record<string, WorkflowNodeRun>;
}>();

const emit = defineEmits<{
  'update:selectedNodeId': [nodeId: string | undefined];
  batchDeleteStoryboards: [ids: number[]];
  batchGenerateVideoPrompts: [tracks: any[]];
  batchGenerateVideos: [tracks: any[]];
  batchDownloadVideos: [tracks: any[]];
  cancelStoryboards: [];
  cancelNode: [nodeId: string];
  cancelTrackVideo: [video: any];
  deleteTrackVideo: [video: any];
  editAsset: [asset: ToonflowApi.Asset];
  editStoryboard: [storyboard: ToonflowApi.Storyboard];
  editStoryboardImage: [storyboard: ToonflowApi.Storyboard];
  exportStoryboardImages: [ids: number[]];
  exportVideo: [videoIds: number[]];
  generateStoryboards: [ids: number[], compulsory: boolean];
  generateDerivedAsset: [asset: ToonflowApi.Asset];
  insertStoryboardAfter: [storyboard: ToonflowApi.Storyboard];
  generateTrackVideo: [track: any];
  generateVideoPrompt: [track: any];
  openVideoTrack: [trackId: number];
  openAgentRun: [runId: number];
  refreshWorkbench: [];
  retryStoryboards: [];
  retryNode: [nodeId: string];
  retryTrackVideo: [video: any, track: any];
  runNode: [nodeId: string, config: Record<string, unknown>];
  runSequence: [nodeIds: string[]];
  savePositions: [positions: Record<string, { x: number; y: number }>];
  saveWorkflow: [workflow: ProductionWorkflowDefinition];
  saveVideoPrompt: [track: any];
  updateContinuityMode: [track: any];
  selectTrackVideo: [track: any, video: any];
  updateFlowSection: [key: 'scriptPlan' | 'storyboardTable', value: string];
  removeStoryboard: [storyboard: ToonflowApi.Storyboard];
  reorderStoryboards: [ids: number[]];
}>();

const route = useRoute();
const router = useRouter();

const flowInstance = ref<any>();
const spacePressed = ref(false);
const editorOpen = ref(false);
const selectedNodeId = ref<string>();
watch(selectedNodeId, (id) => emit('update:selectedNodeId', id));
const selectedNodeIds = ref<string[]>([]);
const nodeConfigDraft = ref<Record<string, any>>({});
const layoutHistory = ref<Record<string, { x: number; y: number }>[]>([]);
const layoutFuture = ref<Record<string, { x: number; y: number }>[]>([]);
const editorKey = ref<'scriptPlan' | 'storyboardTable'>('scriptPlan');
const editorValue = ref('');
const flowData = computed<Record<string, any>>(() => {
  try {
    return JSON.parse(props.flowText || '{}');
  } catch {
    return {};
  }
});

const directorPlan = computed(() =>
  normalizeProductionDocument(
    flowData.value.scriptPlan || flowData.value.directorPlan,
    'scriptPlan',
  ),
);
const storyboardPlan = computed(() =>
  normalizeProductionDocument(flowData.value.storyboardTable, 'storyboardTable'),
);
const workbenchCover = computed(() => assetFileUrl(selectedWorkbenchCover(props.videoTracks)));

function openVideoWorkbench() {
  void router.push({
    name: 'ToonflowVideoWorkbench',
    params: { id: route.params.id },
    query: props.script?.id ? { scriptId: String(props.script.id) } : undefined,
  });
}
const savedPositions = computed(() =>
  flowData.value.canvas?.layoutVersion === 7 ? flowData.value.canvas.positions || {} : {},
);
const workflow = computed(() => normalizeProductionWorkflow(flowData.value.workflow));
const selectedWorkflowNode = computed(() =>
  workflow.value.nodes.find((node) => node.id === selectedNodeId.value),
);
const selectedNodeMeta = computed(() =>
  workflowNodeMeta(selectedWorkflowNode.value?.type ?? ''),
);

function runtimeState(nodeId: string) {
  return productionNodeRuntime({
    directorPlan: directorPlan.value,
    nodeId,
    nodeRun: props.workflowNodeRuns?.[nodeId],
    script: props.script,
    storyboardPlan: storyboardPlan.value,
    storyboardRunState: props.storyboardRunState,
    storyboards: props.storyboards,
    videoTracks: props.videoTracks,
  });
}
const nodes = computed<Node[]>(() =>
  workflow.value.nodes.map((node) => ({
    id: node.id,
    type: workflowNodeViewType(node.type),
    dragHandle: '.dragHandle',
    position: savedPositions.value[node.id] || node.position,
    data: {
      config: node.config,
      meta: workflowNodeMeta(node.type),
      runtime: runtimeState(node.id),
      workflowType: node.type,
    },
  })),
);
const edges = computed<Edge[]>(() =>
  workflow.value.edges.map((edge) => ({
    ...edge,
    animated: false,
    style: { stroke: 'var(--toon-muted)', strokeWidth: 3 },
  })),
);

function selectNode(event: { node: Node }) {
  selectedNodeId.value = event.node.id;
  const node = workflow.value.nodes.find((item) => item.id === event.node.id);
  nodeConfigDraft.value = {
    ...(node?.config ?? {}),
    ...(node?.type === 'storyboard.image'
      ? {
          compulsory: Boolean(node.config?.compulsory),
          concurrentCount: Number(node.config?.concurrentCount ?? 5),
        }
      : {}),
    ...(node?.type === 'director.plan'
      ? { prompt: String(node.config?.prompt ?? '读取当前剧本与资产，生成完整导演规划并写入工作区。') }
      : {}),
    ...(node?.type === 'storyboard.plan'
      ? { prompt: String(node.config?.prompt ?? '读取剧本、资产和导演规划，生成完整分镜表并写入工作区。') }
      : {}),
    ...(node?.type === 'video.generate'
      ? {
          audio: Boolean(node.config?.audio),
          concurrentCount: Number(node.config?.concurrentCount ?? 2),
          resolution: String(node.config?.resolution ?? '720p'),
        }
      : {}),
  };
}

function updateSelection(selection: { nodes?: Node[] }) {
  selectedNodeIds.value = (selection.nodes ?? []).map((node) => node.id);
}

function runSelectedNodes() {
  const ids = selectedNodeIds.value.filter((id) => workflow.value.nodes.some((node) => node.id === id));
  if (!ids.length) return message.info('请先框选要运行的节点');
  emit('runSequence', ids);
}

const selectedRunningCount = computed(() => selectedNodeIds.value.filter((id) => props.workflowNodeRuns?.[id]?.state === 'running').length);
const selectedRetryableCount = computed(() => selectedNodeIds.value.filter((id) => ['failed', 'cancelled'].includes(props.workflowNodeRuns?.[id]?.state ?? '')).length);
function cancelSelectedNodes() {
  selectedNodeIds.value.filter((id) => props.workflowNodeRuns?.[id]?.state === 'running').forEach((id) => emit('cancelNode', id));
}
function retrySelectedNodes() {
  selectedNodeIds.value.filter((id) => ['failed', 'cancelled'].includes(props.workflowNodeRuns?.[id]?.state ?? '')).forEach((id) => emit('retryNode', id));
}
function selectPath(direction: 'upstream' | 'downstream') {
  const nodeId = selectedNodeId.value;
  if (!nodeId) return message.info('请先点击一个节点');
  const ids = workflowPath(workflow.value, nodeId, direction);
  selectedNodeIds.value = ids;
  flowInstance.value?.getNodes?.forEach((node: Node) => { (node as any).selected = ids.includes(node.id); });
}

function saveWorkflow(definition: ProductionWorkflowDefinition) {
  emit('saveWorkflow', definition);
}

function saveNodeConfig(showMessage = true) {
  const selected = selectedWorkflowNode.value;
  if (!selected) return;
  const definition = {
    ...workflow.value,
    nodes: workflow.value.nodes.map((node) =>
      node.id === selected.id ? { ...node, config: { ...nodeConfigDraft.value } } : node,
    ),
  };
  saveWorkflow(definition);
  if (showMessage) message.success('节点配置已保存');
}

function runSelectedNode() {
  const selected = selectedWorkflowNode.value;
  if (!selected || !selectedNodeMeta.value.executable) return;
  saveNodeConfig(false);
  emit('runNode', selected.id, { ...nodeConfigDraft.value });
}

function runSelectedPath(direction: 'downstream' | 'upstream') {
  const selected = selectedWorkflowNode.value;
  if (!selected) return;
  saveNodeConfig(false);
  emit('runSequence', workflowPath(workflow.value, selected.id, direction));
}

function connectNodes(connection: Connection) {
  const source = connection.source;
  const target = connection.target;
  if (!source || !target || source === target) {
    message.warning('不能把节点连接到自身');
    return;
  }
  if (workflow.value.edges.some((edge) => edge.source === source && edge.target === target)) {
    message.info('这两个节点已经连接');
    return;
  }
  if (workflow.value.edges.some((edge) => edge.target === target)) {
    message.warning('一个输入端口只能连接一个上游节点，请先删除原连线');
    return;
  }
  const definition: ProductionWorkflowDefinition = {
    ...workflow.value,
    edges: [
      ...workflow.value.edges,
      { id: `${source}-${target}-${Date.now()}`, source, target },
    ],
  };
  if (workflowHasCycle(definition)) {
    message.error('该连线会形成循环依赖');
    return;
  }
  saveWorkflow(definition);
}

function deleteEdges(deleted: Edge[]) {
  const deletedIds = new Set(deleted.map((edge) => edge.id));
  saveWorkflow({
    ...workflow.value,
    edges: workflow.value.edges.filter((edge) => !deletedIds.has(edge.id)),
  });
}

function deleteNodes(deleted: Node[]) {
  const deletedIds = new Set(deleted.map((node) => node.id));
  saveWorkflow({
    ...workflow.value,
    nodes: workflow.value.nodes.filter((node) => !deletedIds.has(node.id)),
    edges: workflow.value.edges.filter((edge) => !deletedIds.has(edge.source) && !deletedIds.has(edge.target)),
  });
  selectedNodeIds.value = selectedNodeIds.value.filter((id) => !deletedIds.has(id));
  if (selectedNodeId.value && deletedIds.has(selectedNodeId.value)) selectedNodeId.value = undefined;
}

function arrangeNodes() {
  const defaults = defaultProductionWorkflow();
  const defaultPositions = new Map(defaults.nodes.map((node) => [node.id, node.position]));
  const definition = {
    ...workflow.value,
    nodes: workflow.value.nodes.map((node, index) => ({
      ...node,
      position: defaultPositions.get(node.id) ?? { x: index * 1000, y: 0 },
    })),
  };
  saveWorkflow(definition);
  emit(
    'savePositions',
    Object.fromEntries(definition.nodes.map((node) => [node.id, node.position])),
  );
  window.setTimeout(() => flowInstance.value?.fitView?.({ duration: 350, padding: 0.08 }), 50);
}

async function validateCurrentWorkflow() {
  try {
    const result = await validateWorkflow(workflow.value as unknown as Record<string, any>);
    const labels = result.executionOrder.map((id) =>
      workflowNodeMeta(workflow.value.nodes.find((node) => node.id === id)?.type ?? '').label,
    );
    message.success(`工作流有效：${labels.join(' → ')}`);
  } catch (error) {
    message.error(error instanceof Error ? error.message : '工作流校验失败');
  }
}

function openEditor(key: 'scriptPlan' | 'storyboardTable') {
  editorKey.value = key;
  editorValue.value = key === 'scriptPlan' ? directorPlan.value : storyboardPlan.value;
  editorOpen.value = true;
}
function saveEditor() {
  emit('updateFlowSection', editorKey.value, editorValue.value);
  editorOpen.value = false;
}
function saveNodePositions(event: any) {
  const positions: Record<string, { x: number; y: number }> = {
    ...Object.fromEntries(workflow.value.nodes.map((node) => [node.id, node.position])),
    ...savedPositions.value,
  };
  for (const node of flowInstance.value?.getNodes || []) {
    positions[node.id] = { x: node.position.x, y: node.position.y };
  }
  for (const node of event?.nodes || []) {
    positions[node.id] = { x: node.position.x, y: node.position.y };
  }
  if (event?.node) positions[event.node.id] = event.node.position;
  emit('savePositions', positions);
  rememberLayout();
}

function focusStage(stageId: string) {
  void flowInstance.value?.fitView({
    duration: 350,
    maxZoom: 1,
    nodes: [{ id: stageId }],
    padding: 0.12,
  });
}

function agentForNode(type: string) {
  if (type.startsWith('director.') || type.startsWith('storyboard.')) return 'ProductionAgent · 导演/分镜子 Agent';
  if (type.startsWith('asset.') || type.startsWith('image.')) return 'ProductionAgent · 资产/图片子 Agent';
  if (type.startsWith('video.')) return 'ProductionAgent · 视频生产子 Agent';
  return 'ProductionAgent';
}

function saveCanvasLayout() {
  saveNodePositions({ nodes: flowInstance.value?.getNodes || [] });
}

function currentCanvasPositions() {
  return Object.fromEntries((flowInstance.value?.getNodes || []).map((node: any) => [node.id, { x: node.position.x, y: node.position.y }]));
}

function rememberLayout() {
  const current = currentCanvasPositions();
  const previous = layoutHistory.value.at(-1);
  if (previous && JSON.stringify(previous) === JSON.stringify(current)) return;
  layoutHistory.value = [...layoutHistory.value.slice(-29), current];
  layoutFuture.value = [];
}

function restoreLayout(positions: Record<string, { x: number; y: number }>) {
  for (const node of flowInstance.value?.getNodes || []) {
    if (positions[node.id]) node.position = { ...positions[node.id] };
  }
  emit('savePositions', positions);
}

function undoLayout() {
  if (layoutHistory.value.length < 2) return;
  const current = layoutHistory.value.at(-1)!;
  const previous = layoutHistory.value.at(-2)!;
  layoutFuture.value = [...layoutFuture.value, current];
  layoutHistory.value = layoutHistory.value.slice(0, -1);
  restoreLayout(previous);
}

function redoLayout() {
  const next = layoutFuture.value.at(-1);
  if (!next) return;
  layoutFuture.value = layoutFuture.value.slice(0, -1);
  layoutHistory.value = [...layoutHistory.value, next];
  restoreLayout(next);
}

function focusNode(nodeId: string) {
  const node = workflow.value.nodes.find((item) => item.id === nodeId);
  if (!node) return;
  selectNode({ node: { id: nodeId } as Node });
  void flowInstance.value?.fitView?.({ nodes: [nodeId], maxZoom: 0.8, padding: 0.2, duration: 350 });
}

watch([() => props.selectedNodeId, () => props.flowText, flowInstance], () => {
  if (props.selectedNodeId && props.selectedNodeId !== selectedNodeId.value && workflow.value.nodes.some((node) => node.id === props.selectedNodeId)) focusNode(props.selectedNodeId);
}, { immediate: true, flush: 'post' });
watch(flowInstance, () => { if (selectedNodeId.value) focusNode(selectedNodeId.value); });
defineExpose({ focusNode, focusStage });

function handleSpaceDown(event: KeyboardEvent) {
  const target = event.target as HTMLElement | null;
  if (target?.matches('input, textarea, [contenteditable="true"]')) return;
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {
    event.preventDefault();
    saveCanvasLayout();
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'a') {
    event.preventDefault();
    selectedNodeIds.value = workflow.value.nodes.map((node) => node.id);
    flowInstance.value?.getNodes?.forEach((node: Node) => { (node as any).selected = true; });
    return;
  }
  if (event.key === 'Escape') {
    selectedNodeIds.value = [];
    flowInstance.value?.getNodes?.forEach((node: Node) => { (node as any).selected = false; });
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'z') {
    event.preventDefault();
    if (event.shiftKey) redoLayout(); else undoLayout();
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'y') {
    event.preventDefault();
    redoLayout();
    return;
  }
  if (event.key === '0') {
    event.preventDefault();
    void flowInstance.value?.fitView?.({ duration: 350, padding: 0.08 });
    return;
  }
  if (event.code !== 'Space' || event.repeat) return;
  event.preventDefault();
  spacePressed.value = true;
}
function handleSpaceUp(event: KeyboardEvent) {
  if (event.code === 'Space') spacePressed.value = false;
}
onMounted(() => {
  window.addEventListener('keydown', handleSpaceDown);
  window.addEventListener('keyup', handleSpaceUp);
});
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleSpaceDown);
  window.removeEventListener('keyup', handleSpaceUp);
});
</script>

<template>
  <div class="production-flow-shell" :class="{ 'space-panning': spacePressed }">
    <div class="workflow-toolbar">
      <Space class="workflow-toolbar-actions" :size="6">
        <Typography.Text class="workflow-toolbar-title" strong>节点工作流</Typography.Text>
        <Tag>{{ workflow.nodes.length }} 节点</Tag>
        <Tag>{{ workflow.edges.length }} 连线</Tag>
        <Tag v-if="selectedNodeIds.length" color="blue">已选 {{ selectedNodeIds.length }} 节点</Tag>
        <Button size="small" @click="flowInstance?.fitView?.({ duration: 350, padding: 0.08 })">
          适应画布
        </Button>
        <Button size="small" @click="arrangeNodes">整理布局</Button>
        <Button size="small" @click="saveCanvasLayout">保存布局</Button>
        <Button size="small" :disabled="layoutHistory.length < 2" @click="undoLayout">撤销</Button>
        <Button size="small" :disabled="layoutFuture.length === 0" @click="redoLayout">重做</Button>
        <Button size="small" type="primary" ghost @click="validateCurrentWorkflow">校验工作流</Button>
        <Button size="small" :disabled="!selectedNodeIds.length" @click="runSelectedNodes">运行选中节点</Button>
        <Button size="small" danger :disabled="selectedRunningCount === 0" @click="cancelSelectedNodes">取消运行{{ selectedRunningCount ? `（${selectedRunningCount}）` : '' }}</Button>
        <Button size="small" :disabled="selectedRetryableCount === 0" @click="retrySelectedNodes">批量重试{{ selectedRetryableCount ? `（${selectedRetryableCount}）` : '' }}</Button>
        <Button v-if="selectedNodeIds.length" size="small" type="link" @click="selectedNodeIds = []">清除选择</Button>
        <Button size="small" :disabled="!selectedNodeId" @click="selectPath('upstream')">选中上游</Button>
        <Button size="small" :disabled="!selectedNodeId" @click="selectPath('downstream')">选中下游</Button>
      </Space>
    </div>
    <VueFlow
      :nodes="nodes"
      :edges="edges"
      :min-zoom="0.35"
      :max-zoom="1.4"
      :default-viewport="{ x: 20, y: 82, zoom: 0.68 }"
      :pan-on-scroll="true"
      :zoom-on-scroll="false"
      :nodes-draggable="!spacePressed"
      :selection-on-drag="true"
      multi-selection-key-code="Shift"
      :pan-on-drag="spacePressed ? [0] : true"
      :nodes-connectable="true"
      :edges-updatable="false"
      :delete-key-code="['Backspace', 'Delete']"
      class="production-flow"
      @connect="connectNodes"
      @edges-delete="deleteEdges"
      @nodes-delete="deleteNodes"
      @init="flowInstance = $event"
      @node-click="selectNode"
      @selection-change="updateSelection"
      @node-drag-stop="saveNodePositions"
    >
      <Background :gap="18" :size="1" pattern-color="var(--toon-dot)" />
      <Controls position="bottom-left" />
      <MiniMap pannable zoomable position="bottom-right" />

      <template #node-default="{ data }">
        <section class="flow-stage-node flow-unknown-node">
          <Handle id="input" type="target" :position="Position.Left" />
          <span class="port-label port-label-input">{{ data.meta.input }}</span>
          <Handle id="output" type="source" :position="Position.Right" />
          <span class="port-label port-label-output">{{ data.meta.output }}</span>
          <header class="stage-header dragHandle">
            <div>自定义节点</div>
            <Tag :color="data.runtime.color">{{ data.runtime.label }}</Tag>
          </header>
          <div class="stage-waiting">当前前端版本尚未注册此节点视图</div>
        </section>
      </template>

      <template #node-script="{ data }">
        <section class="flow-stage-node stage-script">
          <Handle id="output" type="source" :position="Position.Right" />
          <span class="port-label port-label-output">{{ data.meta.output }}</span>
          <header class="stage-header dragHandle">
            <div><span class="stage-index">1</span> 剧本与衍生资产</div>
            <Tag :color="data.runtime.color">{{ data.runtime.label }}</Tag>
          </header>
          <div v-if="script" class="script-body nopan nodrag">
            <div class="script-summary-header">
              <h3>{{ script.name }}</h3>
              <Button size="small" :aria-expanded="scriptExpanded" @click.stop="scriptExpanded = !scriptExpanded">{{ scriptExpanded ? '收起剧本' : '展开全文' }}</Button>
            </div>
            <pre v-if="scriptExpanded" class="nowheel">{{ script.content }}</pre>
            <p v-else class="script-excerpt">{{ script.content }}</p>
          </div>
          <Empty v-else :image="Empty.PRESENTED_IMAGE_SIMPLE" description="请先选择需要制作的剧本" />
          <section class="embedded-assets" :class="{ waiting: !script }">
            <ProductionAssetStrip :assets="assets" :script="script" @edit="emit('editAsset', $event)" @generate="emit('generateDerivedAsset', $event)" @refresh="emit('refreshWorkbench')" />
          </section>
        </section>
      </template>

      <template #node-scriptPlan="{ data }">
        <section class="flow-stage-node stage-text stage-director" :class="{ waiting: !directorPlan }">
          <Handle id="input" type="target" :position="Position.Left" />
          <span class="port-label port-label-input">{{ data.meta.input }}</span>
          <Handle id="output" type="source" :position="Position.Right" />
          <span class="port-label port-label-output">{{ data.meta.output }}</span>
          <header class="stage-header dragHandle">
            <div>导演规划</div>
            <Space>
              <Tag :color="data.runtime.color">{{ data.runtime.label }}</Tag>
              <Button size="small" type="link" @click.stop="openEditor('scriptPlan')">编辑</Button>
            </Space>
          </header>
          <div v-if="directorPlan" class="director-content nowheel nopan">
            <MarkdownView :content="directorPlan" />
          </div>
          <div v-else class="stage-waiting">Agent 将根据剧本和人物资产生成导演规划</div>
        </section>
      </template>

      <template #node-storyboardTable="{ data }">
        <section class="flow-stage-node stage-table" :class="{ waiting: !storyboardPlan }">
          <Handle id="input" type="target" :position="Position.Left" />
          <span class="port-label port-label-input">{{ data.meta.input }}</span>
          <Handle id="output" type="source" :position="Position.Right" />
          <span class="port-label port-label-output">{{ data.meta.output }}</span>
          <header class="stage-header dragHandle">
            <div>分镜表</div>
            <Space>
              <Tag :color="data.runtime.color">{{ data.runtime.label }}</Tag>
              <Button size="small" type="link" @click.stop="openEditor('storyboardTable')">编辑</Button>
            </Space>
          </header>
          <div v-if="storyboardPlan" class="storyboard-table-content nowheel nopan">
            <MarkdownView :content="storyboardPlan" />
          </div>
          <div v-else class="stage-waiting">导演规划确认后，Agent 将构建结构化分镜表</div>
        </section>
      </template>

      <template #node-storyboard="{ data }">
        <section class="flow-stage-node stage-storyboard" :class="[{ waiting: !storyboards.length }, `runtime-${data.runtime.state}`]">
          <Handle id="input" type="target" :position="Position.Left" />
          <span class="port-label port-label-input">{{ data.meta.input }}</span>
          <Handle id="output" type="source" :position="Position.Right" />
          <span class="port-label port-label-output">{{ data.meta.output }}</span>
          <header class="stage-header dragHandle">
            <div>分镜面板</div>
            <Tag :color="data.runtime.color">{{ data.runtime.label }}</Tag>
          </header>
          <StoryboardPanel
            :busy="storyboardBusy"
            :progress-current="storyboardProgressCurrent"
            :progress-total="storyboardProgressTotal"
            :run-state="storyboardRunState"
            :storyboard-plan="storyboardPlan"
            :storyboards="storyboards"
            @batch-delete="emit('batchDeleteStoryboards', $event)"
            @cancel="emit('cancelStoryboards')"
            @edit="emit('editStoryboard', $event)"
            @edit-image="emit('editStoryboardImage', $event)"
            @export-images="emit('exportStoryboardImages', $event)"
            @generate="(ids, compulsory) => emit('generateStoryboards', ids, compulsory)"
            @insert-after="emit('insertStoryboardAfter', $event)"
            @open-track="emit('openVideoTrack', $event)"
            @remove="emit('removeStoryboard', $event)"
            @reorder="emit('reorderStoryboards', $event)"
            @retry="emit('retryStoryboards')"
          />
        </section>
      </template>

      <template #node-workbench="{ data }">
        <section class="flow-stage-node stage-workbench">
          <Handle id="input" type="target" :position="Position.Left" />
          <span class="port-label port-label-input">{{ data.meta.input }}</span>
          <header class="stage-header dragHandle">
            <div>视频工作台</div>
            <Tag :color="data.runtime.color">{{ data.runtime.label }}</Tag>
          </header>
          <div class="workbench-preview" @click.stop="openVideoWorkbench">
            <video v-if="workbenchCover" :src="workbenchCover" muted preload="none" />
            <div class="workbench-play" aria-hidden="true"><span /></div>
          </div>
        </section>
      </template>
    </VueFlow>
    <Drawer root-class-name="toon-overlay"
      :open="Boolean(selectedWorkflowNode)"
      :title="selectedNodeMeta.label"
      placement="right"
      :width="390"
      @close="selectedNodeId = undefined"
    >
      <div v-if="selectedWorkflowNode" class="node-config-panel">
        <Typography.Paragraph type="secondary">
          {{ selectedNodeMeta.description }}
        </Typography.Paragraph>
        <Descriptions bordered size="small" :column="1">
          <Descriptions.Item label="节点 ID">{{ selectedWorkflowNode.id }}</Descriptions.Item>
          <Descriptions.Item label="节点类型">{{ selectedWorkflowNode.type }}</Descriptions.Item>
          <Descriptions.Item label="执行 Agent">{{ agentForNode(selectedWorkflowNode.type) }}</Descriptions.Item>
          <Descriptions.Item label="运行状态">
            <Tag :color="runtimeState(selectedWorkflowNode.id).color">
              {{ runtimeState(selectedWorkflowNode.id).label }}
            </Tag>
          </Descriptions.Item>
          <Descriptions.Item
            v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.progressTotal"
            label="执行进度"
          >
            {{ workflowNodeRuns[selectedWorkflowNode.id]!.progressCurrent }} /
            {{ workflowNodeRuns[selectedWorkflowNode.id]!.progressTotal }}
          </Descriptions.Item>
          <Descriptions.Item
            v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.attempt"
            label="执行次数"
          >
            {{ workflowNodeRuns[selectedWorkflowNode.id]!.attempt }}
          </Descriptions.Item>
          <Descriptions.Item
            v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.workflowRunId"
            label="工作流运行"
          >
            #{{ workflowNodeRuns[selectedWorkflowNode.id]!.workflowRunId }} · 节点运行 #{{ workflowNodeRuns[selectedWorkflowNode.id]!.id }}
          </Descriptions.Item>
          <Descriptions.Item
            v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.agentRunId"
            label="Agent 运行"
          >
            <Space><span>#{{ workflowNodeRuns[selectedWorkflowNode.id]!.agentRunId }}</span><Button size="small" type="link" @click="emit('openAgentRun', workflowNodeRuns[selectedWorkflowNode.id]!.agentRunId!)">查看事件</Button></Space>
          </Descriptions.Item>
        </Descriptions>
        <Typography.Paragraph
          v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.errorReason"
          class="node-run-error"
          type="danger"
        >
          {{ workflowNodeRuns[selectedWorkflowNode.id]!.errorReason }}
        </Typography.Paragraph>
        <details v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.output" class="node-run-output">
          <summary>查看节点输出</summary>
          <pre>{{ JSON.stringify(workflowNodeRuns[selectedWorkflowNode.id]!.output, null, 2) }}</pre>
        </details>

        <Form v-if="selectedWorkflowNode.type === 'storyboard.image'" class="node-config-form" layout="vertical">
          <Form.Item label="并发生成数量">
            <InputNumber v-model:value="nodeConfigDraft.concurrentCount" :min="1" :max="10" style="width: 100%" />
          </Form.Item>
          <Form.Item label="强制重新生成">
            <Switch v-model:checked="nodeConfigDraft.compulsory" />
          </Form.Item>
          <Descriptions bordered size="small" :column="1">
            <Descriptions.Item label="图片模型">{{ imageModel || '跟随项目配置' }}</Descriptions.Item>
            <Descriptions.Item label="图片质量">{{ imageQuality || '2K' }}</Descriptions.Item>
            <Descriptions.Item label="画面比例">{{ videoRatio || '16:9' }}</Descriptions.Item>
          </Descriptions>
        </Form>

        <Form
          v-else-if="['director.plan', 'storyboard.plan'].includes(selectedWorkflowNode.type)"
          class="node-config-form"
          layout="vertical"
        >
          <Form.Item label="节点执行指令">
            <Input.TextArea v-model:value="nodeConfigDraft.prompt" :rows="6" />
          </Form.Item>
          <Typography.Text
            v-if="selectedWorkflowNode.type === 'storyboard.plan'"
            type="secondary"
          >
            执行分为两步：生成并复审分镜表，然后自动写入分镜面板。重跑成功后会替换旧面板；失败或取消会保留旧版本。
          </Typography.Text>
        </Form>

        <Form v-else-if="selectedWorkflowNode.type === 'video.generate'" class="node-config-form" layout="vertical">
          <Form.Item label="并发生成数量">
            <InputNumber v-model:value="nodeConfigDraft.concurrentCount" :min="1" :max="10" style="width: 100%" />
          </Form.Item>
          <Form.Item label="分辨率">
            <Select
              v-model:value="nodeConfigDraft.resolution"
              :options="[
                { label: '720p', value: '720p' },
                { label: '1080p', value: '1080p' },
              ]"
            />
          </Form.Item>
          <Form.Item label="生成音频">
            <Switch v-model:checked="nodeConfigDraft.audio" />
          </Form.Item>
          <Descriptions bordered size="small" :column="1">
            <Descriptions.Item label="视频模型">{{ videoModel || '跟随项目配置' }}</Descriptions.Item>
            <Descriptions.Item label="生成模式">{{ videoMode || 'startEndRequired' }}</Descriptions.Item>
            <Descriptions.Item label="视频比例">{{ videoRatio || '16:9' }}</Descriptions.Item>
          </Descriptions>
        </Form>

        <div class="node-config-actions">
          <Button @click="saveNodeConfig()">保存配置</Button>
          <Button @click="runSelectedPath('upstream')">运行到此</Button>
          <Button @click="runSelectedPath('downstream')">从此继续</Button>
          <Button
            v-if="workflowNodeRuns?.[selectedWorkflowNode.id]?.state === 'running'"
            danger
            @click="emit('cancelNode', selectedWorkflowNode.id)"
          >取消</Button>
          <Button
            v-if="['cancelled', 'failed'].includes(workflowNodeRuns?.[selectedWorkflowNode.id]?.state || '')"
            @click="emit('retryNode', selectedWorkflowNode.id)"
          >重试</Button>
          <Button
            type="primary"
            :disabled="!selectedNodeMeta.executable || workflowNodeRuns?.[selectedWorkflowNode.id]?.state === 'running'"
            :loading="workflowNodeRuns?.[selectedWorkflowNode.id]?.state === 'running'"
            @click="runSelectedNode"
          >
            {{ selectedNodeMeta.executable ? '运行此节点' : '运行器待接入' }}
          </Button>
        </div>
      </div>
    </Drawer>
    <Modal root-class-name="toon-overlay" v-model:open="editorOpen" :title="editorKey === 'scriptPlan' ? '编辑导演规划' : '编辑分镜表'" width="90vw" @ok="saveEditor">
      <Input.TextArea v-model:value="editorValue" :auto-size="{ minRows: 18, maxRows: 32 }" />
    </Modal>
  </div>
</template>

<style scoped src="./production-flow-canvas.css"></style>
