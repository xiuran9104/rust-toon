import type AgentChat from '../AgentChat.vue';
import type { ProductionWorkflowDefinition } from '../production-workflow';
import type ProductionFlowCanvas from '../ProductionFlowCanvas.vue';

import type { ToonflowApi, WorkflowNodeRun } from '#/api/toonflow';

import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  shallowRef,
  watch,
} from 'vue';
import { useRoute, useRouter } from 'vue-router';

import { useUserStore } from '@vben/stores';
import {
  createProjectLocationStore,
  resolveProjectScript,
} from './project-location';
import { downloadFileFromBlob } from '@vben/utils';

import { message, Modal } from 'ant-design-vue';

import {
  addNovel,
  addScript,
  batchDeleteStoryboards,
  batchGenerateVideoPrompts,
  batchGenerateVideos,
  bindTrackStoryboards,
  cancelTrackVideo,
  cancelWorkflowNodeRun,
  cancelWorkflowRun,
  clearAgentMemory,
  createWorkflowRun,
  deleteNovel,
  deleteScripts,
  deleteTrackVideo,
  downloadStoryboardPreview,
  editStoryboardInfo,
  executeAgentTool,
  exportFinalVideo,
  exportScripts,
  extractScriptAssets,
  generateFlowImage,
  generateNovelEvents,
  generateTrackVideo,
  generateVideoPrompt,
  getAssets,
  getFlowData,
  getImageFlow,
  getLatestWorkflowNodeRun,
  getNovelPage,
  getProject,
  getProjectStatistics,
  getScripts,
  getStoryboards,
  getVideoWorkbench,
  getWorkflowNodeRun,
  getWorkflowRun,
  pollScriptAssets,
  pollStoryboardImages,
  pollTrackVideos,
  previewStoryboardImages,
  removeStoryboard,
  reorderStoryboards,
  retryTrackVideo,
  retryWorkflowNodeRun,
  saveFlowData,
  saveImageFlow,
  saveScriptAgentPlan,
  selectTrackVideo,
  startWorkflowNode,
  updateImageFlow,
  updateStoryboardUrl,
  updateVideoTrackPrompt,
  uploadFlowImage,
} from '#/api/toonflow';

import { assetFileUrl } from '../../assets/asset-types';
import {
  defaultImageFlowEdges,
  directUpstreamNodeIds,
} from '../image-flow-graph';
import { parseNovelText } from '../novel-import';
import {
  extractScriptItems,
  extractXmlContent,
  formatEventDisplay,
  renderMarkdown,
} from '../production-content';
import { requestProductionScriptSwitch } from '../production-script-switch';
import { normalizeProductionWorkflow } from '../production-workflow';
import { storyboardsForTrack as findStoryboardsForTrack } from '../storyboard-track-groups';
import {
  defaultVideoGenerationMode,
  videoFrameItems,
} from '../video-generation-mode';
import {
  projectDetailPanelComponents,
  projectDetailStages,
} from './project-detail-panels';

export function useProjectDetail(fixedProjectId?: number) {
  const route = useRoute();
  const router = useRouter();
  const projectId = computed(() => fixedProjectId ?? Number(route.params.id));
  const user = useUserStore();
  const locationStore = createProjectLocationStore(
    String(user.userInfo?.userId || user.userInfo?.username || 'local'),
    window.localStorage,
  );
  let savedLocation = locationStore.read(projectId.value);
  const requestedStage =
    typeof route.query.stage === 'string' &&
    projectDetailStages.some((stage) => stage.key === route.query.stage)
      ? route.query.stage
      : undefined;
  let locationReady = false;
  const productionNodeId = ref<string>();
  function persistLocation() {
    if (!locationReady) return;
    savedLocation.stage = activeTab.value;
    savedLocation.scriptId = selectedScriptId.value;
    locationStore.write(projectId.value, savedLocation);
  }
  function rememberProductionNode(nodeId?: string) {
    productionNodeId.value = nodeId;
    if (!locationReady || !selectedScriptId.value) return;
    if (nodeId) savedLocation.nodes[String(selectedScriptId.value)] = nodeId;
    else delete savedLocation.nodes[String(selectedScriptId.value)];
    persistLocation();
  }

  const activeTab = ref(requestedStage ?? savedLocation.stage);
  const loadError = ref('');
  const stages = projectDetailStages;
  const activePanelComponent = computed(
    () =>
      projectDetailPanelComponents[activeTab.value] ||
      projectDetailPanelComponents.novel,
  );
  const loading = ref(false);
  const project = ref<ToonflowApi.Project>();
  const imageQuality = computed(() =>
    ['1K', '2K', '4K'].includes(project.value?.imageQuality ?? '')
      ? project.value!.imageQuality
      : '2K',
  );
  const videoMode = computed(() =>
    [
      'endFrameOptional',
      'singleImage',
      'startEndRequired',
      'startFrameOptional',
      'text',
    ].includes(project.value?.mode ?? '')
      ? project.value!.mode
      : 'startEndRequired',
  );
  const statistics = reactive<ToonflowApi.ProjectStatistics>({
    roleCount: 0,
    scriptCount: 0,
    videoCount: 0,
    storyboardCount: 0,
  });
  const novels = shallowRef<ToonflowApi.NovelChapter[]>([]);
  const novelLoading = ref(false);
  const novelPage = ref(1);
  const novelPageSize = ref(10);
  const novelTotal = ref(0);
  const scripts = ref<ToonflowApi.Script[]>([]);
  const assets = ref<ToonflowApi.Asset[]>([]);
  const productionAssets = ref<ToonflowApi.Asset[]>([]);
  const storyboards = ref<ToonflowApi.Storyboard[]>([]);
  const selectedScriptId = ref<number | undefined>(savedLocation.scriptId);
  const selectedScript = computed(() =>
    scripts.value.find((item) => item.id === selectedScriptId.value),
  );
  const flowText = ref(
    '{\n  "script": "",\n  "storyboard": [],\n  "workbench": { "videoList": [] }\n}',
  );
  const loadedFlowProjectId = ref<number>();
  const loadedFlowScriptId = ref<number>();

  const flowImageModalOpen = ref(false);
  const imageFlowId = ref<number>();
  const editingAssetId = ref<number>();
  const editingStoryboardId = ref<number>();
  const editingStoryboard = ref<ToonflowApi.Storyboard>();
  const editingAssetName = ref('');
  const imageFlowNodes = ref<any[]>([]);
  const imageFlowEdges = ref<any[]>([]);
  const imageFlowEditorKey = ref(0);
  const generatingImageNodeId = ref('');
  const storyboardPreviewOpen = ref(false);
  const storyboardPreview = ref('');
  const productionAgentCollapsed = ref(true);
  const productionAgentActivity = ref('等待指令');
  let productionAgentSyncTimer: ReturnType<typeof setInterval> | undefined;
  function stopProductionAgentSync() {
    if (productionAgentSyncTimer) clearInterval(productionAgentSyncTimer);
    productionAgentSyncTimer = undefined;
  }
  let detailActive = true;
  const videoTracks = ref<any[]>([]);
  const trackBindingOpen = ref(false);
  const trackBindingTarget = ref<any>();
  const trackBindingStoryboardIds = ref<number[]>([]);
  const agentType = ref<'productionAgent' | 'scriptAgent'>(
    activeTab.value === 'production' ? 'productionAgent' : 'scriptAgent',
  );
  const scriptPlan = reactive({ storySkeleton: '', adaptationStrategy: '' });

  // Agent workspace tabs - populated in real-time from sub-agent outputs
  const workspaceTabs = reactive<
    { content: string; key: string; label: string }[]
  >([
    { key: 'storySkeleton', label: '故事骨架', content: '' },
    { key: 'adaptationStrategy', label: '改编策略', content: '' },
    { key: 'script', label: '剧本', content: '' },
  ]);
  const workspaceActiveTab = ref('storySkeleton');

  // Pipeline stages for process visualization
  type StageStatus = 'active' | 'completed' | 'pending' | 'review';
  const pipelineStages = reactive<
    { key: string; label: string; status: StageStatus }[]
  >([
    { key: 'init', label: '项目初始化', status: 'completed' },
    { key: 'skeleton', label: '故事骨架', status: 'pending' },
    { key: 'adaptation', label: '改编策略', status: 'pending' },
    { key: 'script', label: '剧本编写', status: 'pending' },
  ]);

  function updatePipelineStage(toolName: string, status: StageStatus) {
    const stageMap: Record<string, string> = {
      storySkeleton: 'skeleton',
      run_sub_agent_storySkeleton: 'skeleton',
      adaptationStrategy: 'adaptation',
      run_sub_agent_adaptationStrategy: 'adaptation',
      script: 'script',
      run_sub_agent_script: 'script',
      save_scripts: 'script',
    };
    const stageKey = stageMap[toolName];
    if (!stageKey) return;
    const idx = pipelineStages.findIndex((s) => s.key === stageKey);
    if (idx >= 0) {
      pipelineStages[idx]!.status = status;
      // Mark previous stages as completed
      for (let i = 0; i < idx; i++) {
        if (pipelineStages[i]!.status === 'active')
          pipelineStages[i]!.status = 'completed';
      }
    }
  }

  function onAgentToolResult(payload: { result: any; toolName: string }) {
    const { toolName, result } = payload;
    if (toolName === 'save_scripts') {
      workspaceActiveTab.value = 'script';
      updatePipelineStage(toolName, 'completed');
      void loadScripts().then(() => {
        workspaceTabs[2]!.content = orderedScripts.value
          .map((script) => `### ${script.name}\n\n${script.content}`)
          .join('\n\n---\n\n');
      });
      return;
    }
    if (
      [
        'add_deriveAsset',
        'del_deriveAsset',
        'generate_deriveAsset',
        'run_sub_agent_derive_assets',
        'run_sub_agent_generate_assets',
      ].includes(toolName)
    ) {
      void loadFlow().then(() => {
        if (
          toolName === 'generate_deriveAsset' ||
          toolName === 'run_sub_agent_generate_assets'
        ) {
          scheduleProductionAssetRefresh();
        }
      });
      return;
    }
    if (
      ['generate_video_prompt', 'select_video', 'update_video_prompt'].includes(
        toolName,
      )
    ) {
      loadFlow();
      return;
    }
    if (
      [
        'add_flowData_storyboard',
        'generate_storyboard',
        'run_sub_agent_director_plan',
        'run_sub_agent_storyboard_gen',
        'run_sub_agent_storyboard_panel',
        'run_sub_agent_storyboard_table',
        'update_storyboard',
      ].includes(toolName)
    ) {
      void loadFlow();
    }
    // Extract workspace content from sub-agent results
    let raw = typeof result === 'string' ? result : JSON.stringify(result);
    // If result is a JSON wrapper like {"agent":"...","content":"..."}, extract content
    try {
      const parsed = JSON.parse(raw);
      if (
        parsed &&
        typeof parsed === 'object' &&
        parsed.content &&
        typeof parsed.content === 'string'
      ) {
        raw = parsed.content;
      }
    } catch {
      /* not JSON, use raw string */
    }
    let content = raw;

    // Match tool name to workspace tab and pipeline stage
    if (toolName.includes('storySkeleton') || toolName.includes('skeleton')) {
      workspaceTabs[0]!.content =
        extractXmlContent(content, 'storySkeleton') || content;
      workspaceActiveTab.value = 'adaptationStrategy';
      updatePipelineStage(toolName, 'completed');
    } else if (
      toolName.includes('adaptationStrategy') ||
      toolName.includes('adaptation')
    ) {
      workspaceTabs[1]!.content =
        extractXmlContent(content, 'adaptationStrategy') || content;
      workspaceActiveTab.value = 'script';
      updatePipelineStage(toolName, 'completed');
    } else if (
      toolName.includes('script') &&
      !toolName.includes('get_script')
    ) {
      workspaceTabs[2]!.content = extractScriptItems(content);
      workspaceActiveTab.value = 'script';
      updatePipelineStage(toolName, 'completed');
    } else if (
      toolName.includes('supervision') ||
      toolName.includes('review')
    ) {
      // Supervision agent reviewing - mark current stage as review
      const currentStage = pipelineStages.find((s) => s.status === 'completed');
      if (currentStage) currentStage.status = 'review';
    }
  }

  // Agent chat messages (WebSocket-driven)
  interface ChatContentBlock {
    type: string;
    id: string;
    data: any;
    status: string;
  }
  interface ChatMessage {
    id: string;
    role: 'assistant' | 'system' | 'user';
    name?: string;
    status: string;
    datetime: string;
    content: ChatContentBlock[];
  }
  const scriptChatMessages = ref<ChatMessage[]>([]);
  const productionChatMessages = ref<ChatMessage[]>([]);
  const agentChatRef = ref<InstanceType<typeof AgentChat> | null>(null);
  const productionAgentChatRef = ref<InstanceType<typeof AgentChat> | null>(
    null,
  );
  const productionFlowCanvasRef = ref<InstanceType<
    typeof ProductionFlowCanvas
  > | null>(null);

  const scriptSearch = ref('');
  const selectedScriptIds = reactive(new Set<number>());
  const scriptUploadInput = ref<HTMLInputElement>();

  const rebuildingStoryboardPanel = ref(false);

  function scriptEpisodeNumber(script: ToonflowApi.Script) {
    const match = script.name.match(/(?:EP|第)\s*0*(\d+)/i);
    return match?.[1] ? Number(match[1]) : Number.POSITIVE_INFINITY;
  }

  const orderedScripts = computed(() =>
    [...scripts.value].sort(
      (left, right) =>
        scriptEpisodeNumber(left) - scriptEpisodeNumber(right) ||
        left.createTime - right.createTime,
    ),
  );
  const visibleScripts = computed(() => {
    const keyword = scriptSearch.value.trim().toLocaleLowerCase();
    return orderedScripts.value.filter(
      (script) =>
        !keyword ||
        [
          script.name,
          script.content,
          ...script.relatedAssets.map((asset) => asset.name),
        ].some((value) => value.toLocaleLowerCase().includes(keyword)),
    );
  });

  function scriptAssetGroups(script: ToonflowApi.Script) {
    const groups = [
      {
        key: 'role',
        label: '角色',
        color: 'blue',
        items: [] as Array<{ id: number; name: string }>,
      },
      {
        key: 'scene',
        label: '场景',
        color: 'green',
        items: [] as Array<{ id: number; name: string }>,
      },
      {
        key: 'tool',
        label: '道具',
        color: 'orange',
        items: [] as Array<{ id: number; name: string }>,
      },
    ];
    for (const related of script.relatedAssets) {
      const asset = assets.value.find(
        (candidate) => candidate.id === related.id,
      );
      const key =
        asset?.type === 'scene'
          ? 'scene'
          : ['costume', 'role'].includes(asset?.type || '')
            ? 'role'
            : 'tool';
      groups.find((group) => group.key === key)!.items.push(related);
    }
    return groups.filter((group) => group.items.length > 0);
  }

  function scriptExtractLabel(script: ToonflowApi.Script) {
    if (script.extractState === 1)
      return `已提取 · ${script.relatedAssets.length} 项资产`;
    if (script.extractState === 0) return '提取中';
    if (script.extractState === 2) return '排队中';
    if (script.extractState === -1) return '提取失败';
    return '待提取';
  }

  function scriptExtractColor(script: ToonflowApi.Script) {
    if (script.extractState === 1) return 'green';
    if (script.extractState === -1) return 'red';
    if (script.extractState === 0) return 'blue';
    if (script.extractState === 2) return 'orange';
    return 'default';
  }

  const scriptOptions = computed(() =>
    orderedScripts.value.map((item) => ({ label: item.name, value: item.id })),
  );

  const assetOptions = computed(() =>
    (productionAssets.value.length > 0
      ? productionAssets.value
      : assets.value
    ).map((item) => {
      const parent = item.parentAssetId
        ? productionAssets.value.find(
            (candidate) => candidate.id === item.parentAssetId,
          )
        : undefined;
      const kind = item.parentAssetId ? '衍生角色' : item.type;
      const owner = parent ? ` · ${parent.name}` : '';
      return {
        label: `${item.name} (${kind}${owner})`,
        value: item.id,
      };
    }),
  );

  const novelColumns = [
    { title: '序号', dataIndex: 'index', width: 72, align: 'center' as const },
    { title: '分卷', dataIndex: 'reel', width: 120, ellipsis: true },
    { title: '章节', dataIndex: 'chapter', width: 180, ellipsis: true },
    { title: '正文摘要', dataIndex: 'chapterData', width: 360 },
    {
      title: '状态',
      dataIndex: 'eventState',
      width: 96,
      align: 'center' as const,
    },
    { title: '事件', dataIndex: 'event', width: 360 },
    { title: '操作', key: 'action', width: 220, fixed: 'right' as const },
  ];

  async function loadProject() {
    const targetProjectId = projectId.value;
    const [projectData, statisticData] = await Promise.all([
      getProject(targetProjectId),
      getProjectStatistics(targetProjectId),
    ]);
    if (!detailActive || targetProjectId !== projectId.value) return;
    project.value = projectData;
    Object.assign(statistics, statisticData);
  }

  let novelLoadVersion = 0;
  async function loadNovels(
    page = novelPage.value,
    pageSize = novelPageSize.value,
  ) {
    const targetProjectId = projectId.value;
    if (!Number.isSafeInteger(targetProjectId) || targetProjectId <= 0) return;
    const targetPage = Math.max(1, Math.trunc(page));
    const targetPageSize = Math.max(1, Math.trunc(pageSize));
    const requestVersion = ++novelLoadVersion;
    novelLoading.value = true;
    try {
      const result = await getNovelPage(
        targetProjectId,
        targetPage,
        targetPageSize,
      );
      if (
        requestVersion !== novelLoadVersion ||
        targetProjectId !== projectId.value
      )
        return;
      const lastPage = Math.max(1, Math.ceil(result.total / targetPageSize));
      if (targetPage > lastPage) {
        await loadNovels(lastPage, targetPageSize);
        return;
      }
      novels.value = result.data;
      novelPage.value = targetPage;
      novelPageSize.value = targetPageSize;
      novelTotal.value = result.total;
    } finally {
      if (requestVersion === novelLoadVersion) novelLoading.value = false;
    }
  }

  function changeNovelPage(page: number, pageSize: number) {
    const nextPageSize = Math.max(1, Math.trunc(pageSize));
    const nextPage =
      nextPageSize === novelPageSize.value ? Math.max(1, Math.trunc(page)) : 1;
    void loadNovels(nextPage, nextPageSize);
  }

  async function loadScripts() {
    const targetProjectId = projectId.value;
    const rows = await getScripts(targetProjectId);
    if (!detailActive || targetProjectId !== projectId.value) return;
    scripts.value = rows;
    selectedScriptId.value = resolveProjectScript(
      orderedScripts.value.map((item) => item.id),
      selectedScriptId.value,
      locationReady ? undefined : route.query.scriptId,
    );
  }

  async function loadAssets() {
    const targetProjectId = projectId.value;
    const rows = await getAssets(targetProjectId);
    if (detailActive && targetProjectId === projectId.value)
      assets.value = rows;
  }

  function isCurrentFlowTarget(
    targetProjectId: number,
    targetScriptId: number,
  ) {
    return (
      detailActive &&
      projectId.value === targetProjectId &&
      selectedScriptId.value === targetScriptId
    );
  }

  async function performLoadFlow() {
    const targetProjectId = projectId.value;
    const targetScriptId = selectedScriptId.value;
    if (!targetScriptId) {
      loadedFlowProjectId.value = undefined;
      loadedFlowScriptId.value = undefined;
      flowText.value =
        '{\n  "script": "",\n  "storyboard": [],\n  "workbench": { "videoList": [] }\n}';
      storyboards.value = [];
      productionAssets.value = [];
      videoTracks.value = [];
      for (const key of Object.keys(workflowNodeRuns))
        delete workflowNodeRuns[key];
      return;
    }
    // These endpoints are independent, read-only snapshots of the same script.
    // Fetch them together so switching production scripts does not pay three
    // consecutive network round trips.
    const [flow, workbench, nextStoryboards] = await Promise.all([
      getFlowData(targetProjectId, targetScriptId),
      getVideoWorkbench(targetProjectId, targetScriptId),
      getStoryboards(targetProjectId, targetScriptId),
    ]);
    if (!isCurrentFlowTarget(targetProjectId, targetScriptId)) return;
    const workflow = normalizeProductionWorkflow(flow.workflow);
    const latestRuns = await Promise.all(
      workflow.nodes.map((node) =>
        getLatestWorkflowNodeRun(targetProjectId, targetScriptId, node.id),
      ),
    );
    if (!isCurrentFlowTarget(targetProjectId, targetScriptId)) return;
    const nextFlowText = JSON.stringify(flow, null, 2);
    const flowKey = `${targetProjectId}:${targetScriptId}`;
    if (!dirtyFlowKeys.has(flowKey) && flowText.value !== nextFlowText) {
      flowText.value = nextFlowText;
    }
    productionAssets.value = Array.isArray(flow.assets) ? flow.assets : [];
    videoTracks.value = workbench.trackList ?? [];
    storyboards.value = nextStoryboards;
    loadedFlowProjectId.value = targetProjectId;
    loadedFlowScriptId.value = targetScriptId;
    const remembered = savedLocation.nodes[String(targetScriptId)];
    productionNodeId.value = workflow.nodes.some(
      (node) => node.id === remembered,
    )
      ? remembered
      : undefined;
    if (remembered && !productionNodeId.value)
      delete savedLocation.nodes[String(targetScriptId)];
    for (const key of Object.keys(workflowNodeRuns))
      delete workflowNodeRuns[key];
    latestRuns.forEach((run, index) => {
      if (run) workflowNodeRuns[workflow.nodes[index]!.id] = run;
    });
    const runningWorkflowNode = latestRuns.find(
      (run) => run?.state === 'running',
    );
    if (runningWorkflowNode) {
      scheduleWorkflowRunPolling(runningWorkflowNode.workflowRunId);
    }
    const latestNodeRun = workflowNodeRuns.storyboard;
    if (latestNodeRun) {
      storyboardNodeRunState.value = latestNodeRun.state;
      storyboardProgressCurrent.value = latestNodeRun.progressCurrent;
      storyboardProgressTotal.value = latestNodeRun.progressTotal;
      if (latestNodeRun.state === 'running') {
        const changedRun = activeStoryboardNodeRunId.value !== latestNodeRun.id;
        activeStoryboardNodeRunId.value = latestNodeRun.id;
        storyboardBusy.value = true;
        if (changedRun) scheduleStoryboardPolling();
      } else if (['cancelled', 'failed'].includes(latestNodeRun.state)) {
        lastStoryboardNodeRunId.value = latestNodeRun.id;
      }
    }
  }

  let loadFlowInFlight: Promise<void> | undefined;
  let loadFlowQueued = false;
  function loadFlow(): Promise<void> {
    if (loadFlowInFlight) {
      loadFlowQueued = true;
      return loadFlowInFlight;
    }
    loadFlowInFlight = (async () => {
      do {
        loadFlowQueued = false;
        await performLoadFlow();
      } while (detailActive && loadFlowQueued);
    })().finally(() => {
      loadFlowInFlight = undefined;
    });
    return loadFlowInFlight;
  }

  let productionAssetRefreshTimer: ReturnType<typeof setTimeout> | undefined;
  let productionAssetRefreshAttempts = 0;

  function scheduleProductionAssetRefresh() {
    if (!detailActive) return;
    if (productionAssetRefreshTimer) clearTimeout(productionAssetRefreshTimer);
    productionAssetRefreshAttempts = 0;
    const refresh = async () => {
      await loadFlow();
      productionAssetRefreshAttempts += 1;
      if (
        detailActive &&
        productionAssetRefreshAttempts < 40 &&
        productionAssets.value.some(
          (asset) =>
            asset.imageState === '生成中' ||
            asset.derive?.some((derived) => derived.imageState === '生成中'),
        )
      ) {
        productionAssetRefreshTimer = setTimeout(refresh, 3000);
      }
    };
    productionAssetRefreshTimer = setTimeout(refresh, 1500);
  }

  let refreshingAll = false;
  async function loadAll() {
    if (loading.value || !detailActive) return;
    if (!Number.isSafeInteger(projectId.value) || projectId.value <= 0) return;
    loading.value = true;
    refreshingAll = true;
    try {
      await Promise.all([
        loadProject(),
        loadNovels(1, novelPageSize.value),
        loadScripts(),
        loadAssets(),
      ]);
      await loadFlow();
      if (!detailActive) return;
      locationReady = true;
      persistLocation();
      loadError.value = '';
    } catch (error) {
      if (detailActive)
        loadError.value =
          error instanceof Error ? error.message : '项目加载失败，请重试';
    } finally {
      refreshingAll = false;
      loading.value = false;
    }
  }

  async function importNovelChapters(
    chapters: ReturnType<typeof parseNovelText>,
  ) {
    const batchSize = 20;
    for (let start = 0; start < chapters.length; start += batchSize) {
      await addNovel(projectId.value, chapters.slice(start, start + batchSize));
    }
  }

  async function importNovelFile(file: File) {
    try {
      const buffer = await file.arrayBuffer();
      let content = new TextDecoder('utf-8').decode(buffer);
      if (content.includes('\uFFFD'))
        content = new TextDecoder('gb18030').decode(buffer);
      const title = file.name.replace(/\.(?:md|txt)$/i, '');
      const chapters = parseNovelText(content, title);
      if (!chapters.length) {
        message.warning('文件中没有可导入的正文');
        return false;
      }
      await importNovelChapters(chapters);
      message.success(`已从文件导入 ${chapters.length} 个章节`);
      await loadNovels();
    } catch (error) {
      message.error(error instanceof Error ? error.message : '文件导入失败');
    }
    return false;
  }

  async function removeNovel(row: any) {
    await deleteNovel(row.id);
    await loadNovels();
  }

  async function extractNovelEvents(row: any) {
    await generateNovelEvents(projectId.value, [row.id]);
    message.success('事件提取任务已提交，可在任务中心查看进度');
    window.setTimeout(loadNovels, 2000);
  }

  async function extractSelectedNovelEvents(ids: number[]) {
    if (!ids.length) {
      message.info('请先选择章节');
      return;
    }
    await generateNovelEvents(projectId.value, ids);
    message.success(`已提交 ${ids.length} 个章节的事件提取任务`);
    window.setTimeout(loadNovels, 2000);
  }

  async function removeScript(script: any) {
    await deleteScripts([script.id]);
    if (selectedScriptId.value === script.id) {
      selectedScriptId.value = undefined;
    }
    await loadScripts();
    await loadFlow();
  }

  function toggleScript(id: number, checked: boolean) {
    checked ? selectedScriptIds.add(id) : selectedScriptIds.delete(id);
  }

  function toggleAllScripts() {
    const ids = visibleScripts.value.map((script) => script.id);
    const shouldSelect = ids.some((id) => !selectedScriptIds.has(id));
    ids.forEach((id) =>
      shouldSelect ? selectedScriptIds.add(id) : selectedScriptIds.delete(id),
    );
  }

  async function batchRemoveScripts() {
    const ids = [...selectedScriptIds];
    if (!ids.length) return;
    await deleteScripts(ids);
    selectedScriptIds.clear();
    await Promise.all([loadScripts(), loadFlow()]);
    message.success(`已删除 ${ids.length} 个剧本`);
  }

  async function batchExtractScriptAssets() {
    const ids = [...selectedScriptIds];
    if (!ids.length) return;
    const result = await extractScriptAssets(projectId.value, ids);
    await loadScripts();
    message.success(
      `已提交 ${ids.length} 个剧本的资产提取任务 #${result.taskId}`,
    );
  }

  async function batchExportScripts() {
    const ids = [...selectedScriptIds];
    if (!ids.length) return;
    const blob = await exportScripts(ids);
    downloadFileFromBlob({
      fileName: `${project.value?.name || 'scripts'}-剧本.zip`,
      source: blob,
    });
  }

  async function importScriptFiles(event: Event) {
    const input = event.target as HTMLInputElement;
    const files = Array.from(input.files || []);
    input.value = '';
    if (!files.length) return;
    for (const [index, file] of files.entries()) {
      const content = await file.text();
      await addScript({
        projectId: projectId.value,
        name: file.name.replace(/\.(?:md|txt)$/i, '') || `剧本 ${index + 1}`,
        content,
        assets: [],
      });
    }
    await loadScripts();
    message.success(`已导入 ${files.length} 个剧本`);
  }

  async function extractAssetsFromScript(script: ToonflowApi.Script) {
    const result = await extractScriptAssets(projectId.value, [script.id]);
    message.success(`资产提取任务 ${result.taskId} 已提交，可在任务中心查看`);
    for (let attempt = 0; attempt < 90; attempt += 1) {
      await new Promise((resolve) => window.setTimeout(resolve, 2000));
      const [result] = await pollScriptAssets([script.id]);
      if (!result || result.extractState === 2 || result.extractState === 0)
        continue;
      await Promise.all([loadScripts(), loadAssets(), loadFlow()]);
      if (result.extractState === 1) {
        message.success(
          `资产提取完成，共识别 ${result.appearanceCount ?? 0} 套人物场景服装/形态，请在分镜制作中确认后生成`,
        );
      } else {
        message.error(result.errorReason || '资产提取失败');
      }
      return;
    }
    message.warning('资产提取仍在处理中，请稍后刷新查看');
  }

  async function generateDerivedAsset(asset: ToonflowApi.Asset) {
    if (!selectedScriptId.value) return message.warning('请先选择制作剧本');
    await executeAgentTool({
      agentType: 'productionAgent',
      projectId: projectId.value,
      scriptId: selectedScriptId.value,
      toolName: 'generate_deriveAsset',
      arguments: { ids: [asset.id], concurrentCount: 1 },
    });
    message.success(`“${asset.name}”已进入生成队列`);
    scheduleProductionAssetRefresh();
  }

  const positionalVideoModes = new Set([
    'endFrameOptional',
    'singleImage',
    'startEndRequired',
    'startFrameOptional',
  ]);
  const explicitFrameRoles = new Set([
    'first',
    'first_frame',
    'last',
    'last_frame',
  ]);

  function storyboardsForTrack(track: any) {
    return findStoryboardsForTrack(storyboards.value, track?.id);
  }

  function storyboardForTrack(track: any) {
    return storyboardsForTrack(track)[0];
  }

  function videoModeForTrack(track: any) {
    return (
      track?.generation?.mode ||
      defaultVideoGenerationMode(
        storyboardsForTrack(track).length,
        videoMode.value,
      )
    );
  }

  function storyboardMediaForTrack(track: any) {
    const medias = Array.isArray(track?.medias) ? track.medias : [];
    return storyboardsForTrack(track).map((storyboard: any) => {
      const media = medias.find(
        (item: any) =>
          item.sources === 'storyboard' &&
          Number(item.id) === Number(storyboard.id),
      );
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

  function videoUploadData(track: any, mode: unknown) {
    const medias = track.medias || [];
    if (typeof mode !== 'string' || !positionalVideoModes.has(mode))
      return medias;
    const storyboardMedias = videoFrameItems(
      storyboardMediaForTrack(track),
      mode,
    );
    const explicitFrameMedias = medias.filter(
      (media: any) =>
        media.sources !== 'storyboard' &&
        explicitFrameRoles.has(media.frameRole),
    );
    return [...storyboardMedias, ...explicitFrameMedias];
  }

  function ensureVideoStoryboardImagesCurrent(tracks: any[]) {
    const invalid = tracks.flatMap((track) =>
      storyboardsForTrack(track).filter((storyboard: any) => {
        const hasImage = Boolean(storyboard.filePath || storyboard.src);
        return hasImage && storyboard.sceneConsistencyStatus !== 'ready';
      }),
    );
    if (invalid.length > 0) {
      message.warning(
        `有 ${invalid.length} 张分镜图的场景母版或状态已变化，请先重新生成分镜图片`,
      );
      return false;
    }
    return true;
  }

  async function generateSelectedVideoPrompts(tracks: any[]) {
    if (!project.value?.videoModel)
      return message.warning('请先配置项目视频模型');
    const mode = videoModeForTrack(tracks[0]);
    tracks.forEach((track) => {
      track.promptGenerating = true;
    });
    try {
      await batchGenerateVideoPrompts({
        projectId: projectId.value,
        trackData: tracks.map((track) => ({
          trackId: track.id,
          info: track.medias || [],
        })),
        mode,
        model: project.value.videoModel,
        concurrentCount: 5,
      });
      message.success(`已提交 ${tracks.length} 条提示词任务`);
      window.setTimeout(loadFlow, 2500);
    } finally {
      tracks.forEach((track) => {
        track.promptGenerating = false;
      });
    }
  }

  async function generateSelectedVideos(tracks: any[]) {
    if (!selectedScriptId.value || !project.value?.videoModel)
      return message.warning('请先选择剧本并配置视频模型');
    if (!ensureVideoStoryboardImagesCurrent(tracks)) return;
    const firstTrack = tracks[0];
    const first = firstTrack?.generation || {};
    const mode = first.mode || videoModeForTrack(firstTrack);
    tracks.forEach((track) => {
      track.videoGenerating = true;
    });
    try {
      await batchGenerateVideos({
        projectId: projectId.value,
        scriptId: selectedScriptId.value,
        trackData: tracks.map((track) => {
          const storyboard = storyboardForTrack(track);
          return {
            trackId: track.id,
            uploadData: videoUploadData(track, mode),
            prompt: track.prompt || storyboard?.videoDesc || '',
            duration: track.duration || storyboard?.duration || 4,
            continuityMode:
              track.generation?.continuityMode ||
              track.continuityMode ||
              first.continuityMode ||
              'auto',
          };
        }),
        model: first.model || project.value.videoModel,
        mode,
        continuityMode: first.continuityMode || 'auto',
        resolution: first.resolution || '1080p',
        audio: first.audio !== false,
      });
      message.success(`已提交 ${tracks.length} 个视频生成任务`);
      window.setTimeout(loadFlow, 2500);
    } catch (error) {
      tracks.forEach((track) => {
        track.videoGenerating = false;
      });
      throw error;
    }
  }

  function downloadSelectedVideos(tracks: any[]) {
    let count = 0;
    tracks.forEach((track, index) => {
      const videos = track.videoList || [];
      const video =
        videos.find(
          (item: any) => item.id === track.selectVideoId && item.src,
        ) || videos.find((item: any) => item.src);
      if (!video?.src) return;
      const link = document.createElement('a');
      link.href = assetFileUrl(video.src);
      link.download = `track-${index + 1}.mp4`;
      link.click();
      count += 1;
    });
    count
      ? message.success(`已开始下载 ${count} 个视频`)
      : message.warning('选中的轨道暂无可下载视频');
  }

  async function loadAgentMemory() {
    // No-op: backend sends history via WebSocket on connect
  }

  async function resetAgentWorkspace() {
    // Clear chat messages
    if (agentType.value === 'productionAgent') {
      productionChatMessages.value = [];
    } else {
      scriptChatMessages.value = [];
    }
    // Reset workspace tabs
    workspaceTabs.forEach((t) => (t.content = ''));
    workspaceActiveTab.value = 'storySkeleton';
    // Reset pipeline stages
    pipelineStages.forEach(
      (s) => (s.status = s.key === 'init' ? 'completed' : 'pending'),
    );
    // Clear server-side memory
    try {
      const agentIsolationKey = `${agentType.value}:${projectId.value}:${agentType.value === 'productionAgent' ? (selectedScriptId.value ?? 'none') : 'project'}`;
      await clearAgentMemory(agentType.value, agentIsolationKey);
    } catch {
      /* ignore */
    }
    // Reconnect WebSocket
    agentChatRef.value?.disconnect();
    setTimeout(() => agentChatRef.value?.connect(), 200);
    message.success('已重新开始');
  }

  async function resetProductionAgent() {
    if (!selectedScriptId.value) {
      message.warning('请先选择剧本');
      return;
    }
    const isolationKey = `productionAgent:${projectId.value}:${selectedScriptId.value}`;
    await clearAgentMemory('productionAgent', isolationKey);
    productionChatMessages.value = [];
    productionAgentChatRef.value?.disconnect();
    window.setTimeout(() => productionAgentChatRef.value?.connect(), 200);
    message.success('当前剧本的分镜制作 Agent 已重新开始');
  }
  function changeProductionScript(
    value: unknown,
    onChanged?: (scriptChanged: boolean) => void,
  ) {
    const scriptId = Number(value);
    if (!Number.isFinite(scriptId)) return;
    requestProductionScriptSwitch({
      confirm: (commit) => {
        Modal.confirm({
          title: 'Agent 正在执行',
          content:
            '切换剧本不会停止当前 Agent，但当前画布将切换到另一份工作区。确认继续吗？',
          okText: '确认切换',
          cancelText: '留在当前剧本',
          onOk: commit,
        });
      },
      getCurrentScriptId: () => selectedScriptId.value,
      messages: productionChatMessages.value,
      onCommit: (targetScriptId, scriptChanged) => {
        selectedScriptId.value = targetScriptId;
        onChanged?.(scriptChanged);
      },
      targetScriptId: scriptId,
    });
  }
  async function clearProductionAgentMemory(
    memoryType: 'all' | 'message' | 'summary',
  ) {
    if (!selectedScriptId.value) return;
    const isolationKey = `productionAgent:${projectId.value}:${selectedScriptId.value}`;
    await clearAgentMemory('productionAgent', isolationKey, memoryType);
    if (memoryType !== 'summary') productionChatMessages.value = [];
    message.success(
      memoryType === 'message'
        ? '消息记忆已清除'
        : memoryType === 'summary'
          ? '摘要记忆已清除'
          : '全部记忆已清除',
    );
  }

  async function clearScriptAgentMemory(
    memoryType: 'all' | 'message' | 'summary',
  ) {
    const isolationKey = `scriptAgent:${projectId.value}:project`;
    await clearAgentMemory('scriptAgent', isolationKey, memoryType);
    if (memoryType !== 'summary') scriptChatMessages.value = [];
    message.success(
      memoryType === 'message'
        ? '消息记忆已清除'
        : memoryType === 'summary'
          ? '摘要记忆已清除'
          : '全部记忆已清除',
    );
  }

  async function saveAgentWorkspace() {
    await saveScriptAgentPlan(projectId.value, {
      ...scriptPlan,
      script: scripts.value.map(({ id, name, content }) => ({
        id,
        name,
        content,
      })),
    });
    message.success('剧本 Agent 工作区已保存');
  }

  function openScriptGeneration() {
    agentType.value = 'scriptAgent';
    scriptChatMessages.value = [];
    agentChatRef.value?.connect();
    setTimeout(() => {
      agentChatRef.value?.send(
        '你好，请先读取当前项目中已完成事件提取的章节和已有剧本，分析项目状态。若改编参数尚未确认，请先给出一套包含总集数、单集时长、明确原著起止章节和付费策略的具体推荐配置，再让我逐项确认；确认完成前不得生成故事骨架、改编策略或剧本。',
      );
    }, 300);
  }

  async function saveFlowText() {
    const targetProjectId = projectId.value;
    const targetScriptId = selectedScriptId.value;
    if (!targetScriptId) {
      message.warning('请先选择剧本');
      return;
    }
    if (
      loadedFlowProjectId.value !== targetProjectId ||
      loadedFlowScriptId.value !== targetScriptId
    ) {
      message.warning('当前剧本的制作数据仍在加载，请稍后再保存');
      return;
    }
    let data: Record<string, any>;
    try {
      data = JSON.parse(flowText.value);
    } catch {
      message.error('Flow JSON 格式不正确');
      return;
    }
    await saveFlowData(targetProjectId, targetScriptId, data);
    if (
      projectId.value === targetProjectId &&
      selectedScriptId.value === targetScriptId
    ) {
      message.success('生产工作流已保存');
    }
  }

  const flowCanvasSaveTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const flowSaveChains = new Map<string, Promise<void>>();
  const dirtyFlowKeys = new Set<string>();

  function enqueueFlowSave(
    targetProjectId: number,
    targetScriptId: number,
    data: Record<string, any>,
  ) {
    const targetKey = `${targetProjectId}:${targetScriptId}`;
    dirtyFlowKeys.add(targetKey);
    const previous = flowSaveChains.get(targetKey) ?? Promise.resolve();
    const next = previous
      .catch(() => undefined)
      .then(() => saveFlowData(targetProjectId, targetScriptId, data));
    flowSaveChains.set(targetKey, next);
    void next
      .then(() => {
        if (flowSaveChains.get(targetKey) === next)
          dirtyFlowKeys.delete(targetKey);
      })
      .finally(() => {
        if (flowSaveChains.get(targetKey) === next) {
          flowSaveChains.delete(targetKey);
        }
      })
      .catch(() => undefined);
    return next;
  }
  function flowObject() {
    try {
      return JSON.parse(flowText.value || '{}') as Record<string, any>;
    } catch {
      return {};
    }
  }
  function persistFlowObject(data: Record<string, any>) {
    const targetProjectId = projectId.value;
    const targetScriptId = selectedScriptId.value;
    if (
      !targetScriptId ||
      loadedFlowProjectId.value !== targetProjectId ||
      loadedFlowScriptId.value !== targetScriptId
    )
      return;
    flowText.value = JSON.stringify(data, null, 2);
    const targetKey = `${targetProjectId}:${targetScriptId}`;
    const pendingSave = flowCanvasSaveTimers.get(targetKey);
    if (pendingSave) clearTimeout(pendingSave);
    const saveTimer = setTimeout(() => {
      flowCanvasSaveTimers.delete(targetKey);
      void enqueueFlowSave(targetProjectId, targetScriptId, data).catch(() => {
        message.error('自动保存失败，修改已保留，请重试');
      });
    }, 500);
    flowCanvasSaveTimers.set(targetKey, saveTimer);
  }
  function saveProductionCanvasPositions(
    positions: Record<string, { x: number; y: number }>,
  ) {
    const data = flowObject();
    data.canvas = { ...(data.canvas || {}), layoutVersion: 7, positions };
    const workflow = normalizeProductionWorkflow(data.workflow);
    workflow.nodes = workflow.nodes.map((node) => ({
      ...node,
      position: positions[node.id] ?? node.position,
    }));
    data.workflow = workflow;
    persistFlowObject(data);
  }
  function saveProductionWorkflow(workflow: ProductionWorkflowDefinition) {
    const data = flowObject();
    data.workflow = workflow;
    data.canvas = {
      ...(data.canvas || {}),
      layoutVersion: 7,
      positions: Object.fromEntries(
        workflow.nodes.map((node) => [node.id, node.position]),
      ),
    };
    persistFlowObject(data);
  }
  async function updateProductionFlowSection(
    key: 'scriptPlan' | 'storyboardTable',
    value: string,
  ) {
    const data = flowObject();
    data[key] = value;
    const targetProjectId = projectId.value;
    const targetScriptId = selectedScriptId.value;
    if (!targetScriptId) return;
    flowText.value = JSON.stringify(data, null, 2);
    const targetKey = `${targetProjectId}:${targetScriptId}`;
    const pendingSave = flowCanvasSaveTimers.get(targetKey);
    if (pendingSave) {
      clearTimeout(pendingSave);
      flowCanvasSaveTimers.delete(targetKey);
    }
    try {
      await enqueueFlowSave(targetProjectId, targetScriptId, data);
      message.success(key === 'scriptPlan' ? '导演规划已保存' : '分镜表已保存');
    } catch {
      dirtyFlowKeys.add(targetKey);
      message.error('保存失败，修改已保留，请重试');
    }
  }
  function previewProductionFlowSection(payload: {
    key: 'scriptPlan' | 'storyboardTable';
    value: string;
  }) {
    const data = flowObject();
    data[payload.key] = payload.value;
    flowText.value = JSON.stringify(data, null, 2);
  }
  function onProductionAgentActivity(payload: {
    status: string;
    toolName: string;
  }) {
    const names: Record<string, string> = {
      run_sub_agent_derive_assets: '分析衍生资产',
      run_sub_agent_generate_assets: '生成衍生资产',
      run_sub_agent_director_plan: '生成导演规划',
      run_sub_agent_storyboard_table: '生成分镜表',
      run_sub_agent_storyboard_panel: '构建分镜面板',
      run_sub_agent_storyboard_gen: '生成分镜图',
      run_sub_agent_supervision: '监制检查',
      generate_storyboard: '生成分镜图',
    };
    const toolStages: Record<string, string> = {
      add_deriveAsset: 'script',
      del_deriveAsset: 'script',
      generate_deriveAsset: 'script',
      run_sub_agent_derive_assets: 'script',
      run_sub_agent_generate_assets: 'script',
      run_sub_agent_director_plan: 'scriptPlan',
      run_sub_agent_storyboard_table: 'storyboardTable',
      run_sub_agent_supervision: 'storyboardTable',
      run_sub_agent_storyboard_panel: 'storyboard',
      run_sub_agent_storyboard_gen: 'storyboard',
      generate_storyboard: 'storyboard',
      generate_video_prompt: 'workbench',
      generate_video: 'workbench',
    };
    if (payload.status === 'pending' || payload.status === 'streaming') {
      const stage = toolStages[payload.toolName];
      if (stage)
        nextTick(() => productionFlowCanvasRef.value?.focusStage(stage));
    }
    if (payload.toolName === '__agent__') {
      if (payload.status === 'pending' || payload.status === 'streaming') {
        if (!productionAgentSyncTimer) {
          productionAgentSyncTimer = setInterval(() => void loadFlow(), 1800);
        }
        productionAgentActivity.value = 'Agent 执行中';
      } else {
        stopProductionAgentSync();
        void loadFlow();
        productionAgentActivity.value =
          payload.status === 'error' ? 'Agent 执行失败' : 'Agent 已完成';
      }
      return;
    }
    const name = names[payload.toolName] || payload.toolName;
    productionAgentActivity.value =
      payload.status === 'complete'
        ? `${name}完成`
        : payload.status === 'error'
          ? `${name}失败`
          : name;
  }

  function collectUpstreamNodeIds(nodeId: string) {
    return directUpstreamNodeIds(imageFlowEdges.value, nodeId);
  }

  async function createFlowImage(nodeId: string) {
    if (!project.value?.imageModel)
      return message.warning('请先配置项目图片模型');
    const generatedNode = imageFlowNodes.value.find(
      (node) => node.id === nodeId && node.type === 'generated',
    );
    if (!generatedNode) return;
    const upstreamIds = collectUpstreamNodeIds(nodeId);
    const upstream = imageFlowNodes.value.filter((node) =>
      upstreamIds.has(node.id),
    );
    const references = upstream
      .flatMap((node) => [node.data.image, node.data.generatedImage])
      .filter(Boolean);
    const prompt = [
      ...upstream
        .filter((node) => node.type === 'prompt')
        .map((node) => node.data.prompt),
      generatedNode.data.prompt,
    ]
      .filter(Boolean)
      .join('\n');
    if (!references.length) return message.warning('请先连接至少一个图片节点');
    if (!prompt.trim())
      return message.warning('请先连接编辑指令节点或填写节点编辑要求');
    generatingImageNodeId.value = nodeId;
    try {
      const result = await generateFlowImage({
        projectId: projectId.value,
        storyboardId: editingStoryboardId.value,
        model: String(project.value.imageModel),
        quality: imageQuality.value,
        ratio: project.value.videoRatio || '16:9',
        prompt,
        references,
        targetType: generatedNode.data.targetType || 'storyboard',
      });
      generatedNode.data.generatedImage = result.url;
      generatedNode.data.references = references;
      generatedNode.data.sceneStateId = result.sceneStateId;
      generatedNode.data.sceneGenerationContext = result.sceneGenerationContext;
      message.success('当前节点图片已生成');
    } finally {
      generatingImageNodeId.value = '';
    }
  }

  function addImageFlowNode(type: 'generated' | 'prompt' | 'upload') {
    const id = `${type}-${Date.now()}`;
    const editingAsset = productionAssets.value.find(
      (asset) => asset.id === editingAssetId.value,
    );
    const defaultTargetType = editingStoryboardId.value
      ? 'storyboard'
      : editingAsset?.type === 'scene'
        ? 'scene'
        : 'role';
    imageFlowNodes.value.push({
      id,
      type,
      position: { x: imageFlowNodes.value.length * 260, y: 0 },
      data:
        type === 'upload'
          ? { image: '' }
          : type === 'prompt'
            ? { prompt: '' }
            : {
                generatedImage: '',
                prompt: '',
                references: [],
                targetType: defaultTargetType,
              },
    });
    if (imageFlowNodes.value.length > 1) {
      const source = imageFlowNodes.value.at(-2)?.id;
      imageFlowEdges.value.push({
        id: `edge-${source}-${id}`,
        source,
        target: id,
      });
    }
  }

  async function openAssetImageFlow(asset: ToonflowApi.Asset) {
    if (!selectedScriptId.value) return message.warning('请先选择剧本');
    productionFlowCanvasRef.value?.focusNode('script');
    editingAssetId.value = asset.id;
    editingStoryboardId.value = undefined;
    editingAssetName.value = asset.name;
    imageFlowId.value = asset.flowId;
    imageFlowNodes.value = [];
    imageFlowEdges.value = [];
    imageFlowEditorKey.value += 1;
    if (imageFlowId.value) {
      const flow = await getImageFlow(imageFlowId.value);
      imageFlowNodes.value = flow?.nodes ?? [];
      imageFlowEdges.value = flow?.edges ?? [];
    }
    if (imageFlowNodes.value.length === 0) {
      const parentAsset = asset.parentAssetId
        ? productionAssets.value.find((item) => item.id === asset.parentAssetId)
        : undefined;
      const sourceImage = parentAsset?.imageFilePath || asset.imageFilePath;
      const initialPrompt =
        asset.prompt || asset.description || asset.remark || '';
      const suffix = Date.now();
      const sourceId = `upload-${suffix}`;
      const promptId = `prompt-${suffix}`;
      const generatedId = `generated-${suffix}`;
      imageFlowNodes.value = [
        {
          id: sourceId,
          type: 'upload',
          position: { x: 0, y: 0 },
          data: { image: assetFileUrl(sourceImage) },
        },
        {
          id: promptId,
          type: 'prompt',
          position: { x: 330, y: 0 },
          data: { prompt: initialPrompt },
        },
        {
          id: generatedId,
          type: 'generated',
          position: { x: 660, y: 0 },
          data: {
            generatedImage: '',
            prompt: initialPrompt,
            references: [],
            targetType: asset.type === 'scene' ? 'scene' : 'role',
          },
        },
      ];
      imageFlowEdges.value = [
        {
          id: `edge-${sourceId}-${generatedId}`,
          source: sourceId,
          target: generatedId,
        },
        {
          id: `edge-${promptId}-${generatedId}`,
          source: promptId,
          target: generatedId,
        },
      ];
    }
    restoreMissingImageFlowEdges();
    flowImageModalOpen.value = true;
  }

  async function openStoryboardImageFlow(storyboard: ToonflowApi.Storyboard) {
    if (!selectedScriptId.value) return message.warning('请先选择剧本');
    productionFlowCanvasRef.value?.focusNode('storyboard');
    editingAssetId.value = undefined;
    editingStoryboardId.value = storyboard.id;
    editingStoryboard.value = storyboard;
    editingAssetName.value = `分镜 ${storyboard.index ?? storyboard.id}`;
    imageFlowId.value = storyboard.flowId;
    imageFlowNodes.value = [];
    imageFlowEdges.value = [];
    imageFlowEditorKey.value += 1;
    if (imageFlowId.value) {
      const flow = await getImageFlow(imageFlowId.value);
      imageFlowNodes.value = flow?.nodes ?? [];
      imageFlowEdges.value = flow?.edges ?? [];
    }
    if (imageFlowNodes.value.length === 0) {
      const suffix = Date.now();
      const promptId = `prompt-${suffix}`;
      const generatedId = `generated-${suffix}`;
      const referenceNodes = (storyboard.associateAssetsIds ?? []).flatMap(
        (assetId, index) => {
          const asset = productionAssets.value.find(
            (item) => item.id === assetId,
          );
          return asset?.imageFilePath
            ? [
                {
                  id: `upload-asset-${assetId}-${suffix}`,
                  type: 'upload',
                  position: { x: 0, y: (index + 1) * 220 },
                  data: {
                    assetId,
                    assetName: asset.name,
                    assetSlot: index,
                    assetType: asset.type,
                    image: assetFileUrl(asset.imageFilePath),
                  },
                },
              ]
            : [];
        },
      );
      imageFlowNodes.value = [
        ...referenceNodes,
        {
          id: promptId,
          type: 'prompt',
          position: { x: 350, y: 0 },
          data: { prompt: storyboard.prompt || storyboard.videoDesc || '' },
        },
        {
          id: generatedId,
          type: 'generated',
          position: { x: 700, y: 0 },
          data: {
            generatedImage: assetFileUrl(storyboard.filePath || storyboard.src),
            prompt: storyboard.prompt || '',
            references: [],
            targetType: 'storyboard',
          },
        },
      ];
      imageFlowEdges.value = [
        ...referenceNodes.map((node) => ({
          id: `edge-${node.id}-${generatedId}`,
          source: node.id,
          target: generatedId,
        })),
        {
          id: `edge-${promptId}-${generatedId}`,
          source: promptId,
          target: generatedId,
        },
      ];
    }
    const generatedNode = imageFlowNodes.value.find(
      (node) => node.type === 'generated',
    );
    if (generatedNode) {
      generatedNode.data.generatedImage ||= assetFileUrl(
        storyboard.filePath || storyboard.src,
      );
      for (const [index, assetId] of (
        storyboard.associateAssetsIds ?? []
      ).entries()) {
        if (
          imageFlowNodes.value.some(
            (node) => node.type === 'upload' && node.data.assetSlot === index,
          )
        )
          continue;
        const asset = productionAssets.value.find(
          (item) => item.id === assetId,
        );
        if (!asset) continue;
        const nodeId = `upload-asset-${assetId}-${Date.now()}-${index}`;
        imageFlowNodes.value.push({
          id: nodeId,
          type: 'upload',
          position: { x: 0, y: index * 220 },
          data: {
            assetId,
            assetName: asset.name,
            assetSlot: index,
            assetType: asset.type,
            image: assetFileUrl(asset.imageFilePath),
          },
        });
        imageFlowEdges.value.push({
          id: `edge-${nodeId}-${generatedNode.id}`,
          source: nodeId,
          target: generatedNode.id,
        });
      }
    }
    restoreMissingImageFlowEdges();
    flowImageModalOpen.value = true;
  }

  const imageFlowAssetOptions = computed(() =>
    productionAssets.value
      .filter(
        (asset) =>
          !!asset.imageFilePath &&
          (asset.type !== 'role' || !!asset.parentAssetId),
      )
      .map((asset) => ({
        id: asset.id,
        image: assetFileUrl(asset.imageFilePath),
        label: asset.name,
        type: asset.type,
      })),
  );

  async function selectImageFlowAsset(nodeId: string, assetId: number) {
    const node = imageFlowNodes.value.find(
      (item) => item.id === nodeId && item.type === 'upload',
    );
    const asset = productionAssets.value.find((item) => item.id === assetId);
    if (!node || !asset?.imageFilePath) return;
    node.data.assetId = asset.id;
    node.data.assetName = asset.name;
    node.data.assetType = asset.type;
    node.data.image = assetFileUrl(asset.imageFilePath);
    const storyboard = editingStoryboard.value;
    const slot = Number(node.data.assetSlot);
    if (storyboard && Number.isInteger(slot)) {
      const assetIds = [...(storyboard.associateAssetsIds ?? [])];
      assetIds[slot] = asset.id;
      await editStoryboardInfo({
        associateAssetsIds: assetIds,
        duration: storyboard.duration,
        id: storyboard.id,
        prompt: storyboard.prompt || '',
        shouldGenerateImage: storyboard.shouldGenerateImage ?? 1,
        track: storyboard.track,
        videoDesc: storyboard.videoDesc || '',
      });
      storyboard.associateAssetsIds = assetIds;
      message.success(`已将参考资产更换为“${asset.name}”`);
    }
  }

  async function saveVisualImageFlow() {
    const storyboardGeneratedImage = editingStoryboardId.value
      ? [...imageFlowNodes.value]
          .reverse()
          .find((node) => node.type === 'generated' && node.data.generatedImage)
          ?.data.generatedImage
      : undefined;
    if (editingStoryboardId.value && !storyboardGeneratedImage) {
      return message.warning('请先执行图片编辑节点并生成结果');
    }
    if (imageFlowId.value) {
      await updateImageFlow(
        imageFlowId.value,
        imageFlowNodes.value,
        imageFlowEdges.value,
      );
    } else {
      const result = await saveImageFlow(
        imageFlowNodes.value,
        imageFlowEdges.value,
        editingAssetId.value,
      );
      imageFlowId.value = result.id;
    }
    if (editingStoryboardId.value && imageFlowId.value) {
      await updateStoryboardUrl(
        editingStoryboardId.value,
        storyboardGeneratedImage,
        imageFlowId.value,
        [...imageFlowNodes.value]
          .reverse()
          .find((node) => node.type === 'generated' && node.data.generatedImage)
          ?.data.sceneStateId,
        [...imageFlowNodes.value]
          .reverse()
          .find((node) => node.type === 'generated' && node.data.generatedImage)
          ?.data.sceneGenerationContext,
      );
    }
    await loadFlow();
    message.success('图片工作流已保存');
  }

  async function uploadImageFlowReference(nodeId: string, file: File) {
    if (!file || !selectedScriptId.value) return;
    const base64Data = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || ''));
      reader.onerror = () => reject(reader.error);
      reader.readAsDataURL(file);
    });
    const url = await uploadFlowImage(
      projectId.value,
      selectedScriptId.value,
      base64Data,
    );
    const uploadNode = imageFlowNodes.value.find(
      (node) => node.id === nodeId && node.type === 'upload',
    );
    if (uploadNode) uploadNode.data.image = url;
    message.success('参考图上传成功');
  }

  function connectImageFlow(connection: any) {
    if (
      !connection.source ||
      !connection.target ||
      connection.source === connection.target
    )
      return;
    const exists = imageFlowEdges.value.some(
      (edge) =>
        edge.source === connection.source && edge.target === connection.target,
    );
    if (!exists)
      imageFlowEdges.value.push({
        id: `edge-${connection.source}-${connection.target}-${Date.now()}`,
        source: connection.source,
        target: connection.target,
      });
  }

  function restoreMissingImageFlowEdges() {
    if (imageFlowEdges.value.length || imageFlowNodes.value.length < 2) return;
    imageFlowEdges.value = defaultImageFlowEdges(imageFlowNodes.value);
  }

  async function planImageEditWithAgent(nodeId: string) {
    const node = imageFlowNodes.value.find((item) => item.id === nodeId);
    if (!node || !selectedScriptId.value)
      return message.warning('请先选择剧本');
    generatingImageNodeId.value = nodeId;
    try {
      const result = await executeAgentTool({
        agentType: 'productionAgent',
        projectId: projectId.value,
        scriptId: selectedScriptId.value,
        toolName: 'run_sub_agent_image_edit',
        arguments: {
          prompt: `请读取当前剧本、资产和分镜上下文，为一次${node.data.targetType || 'storyboard'}图片编辑生成可直接执行的中文编辑指令。只输出编辑指令，明确需要改变的内容以及必须保持不变的人物身份、构图和视觉元素，不要执行生图。`,
        },
      });
      node.data.prompt = result.result?.content || result.result || '';
      message.success('生产 Agent 已生成当前节点的编辑指令');
    } finally {
      generatingImageNodeId.value = '';
    }
  }

  function removeImageFlowNode(id: string) {
    imageFlowNodes.value = imageFlowNodes.value.filter(
      (node) => node.id !== id,
    );
    imageFlowEdges.value = imageFlowEdges.value.filter(
      (edge) => edge.source !== id && edge.target !== id,
    );
  }

  async function rebuildStoryboardPanel() {
    if (!selectedScriptId.value) {
      message.warning('请先选择剧本');
      return;
    }
    rebuildingStoryboardPanel.value = true;
    try {
      const hasExisting = storyboards.value.length > 0;
      await executeAgentTool({
        agentType: 'productionAgent',
        projectId: projectId.value,
        scriptId: selectedScriptId.value,
        toolName: 'run_sub_agent_storyboard_panel',
        arguments: {
          prompt: hasExisting
            ? '重新读取最新 storyboardTable、assets 和 storyboard。按分镜表顺序逐条核对并修复现有分镜，重点维护同场角色的入场、在场、画外和离场连续性；同步更新 videoDesc、prompt、track、duration、associateAssetsIds 和 shouldGenerateImage。现有分镜数量与分镜表一致时只能调用 update_storyboard，禁止重复新增。'
            : '读取最新 storyboardTable 和 assets，按分镜表完整生成分镜面板；逐镜维护同场角色连续性，并确保画面描述、衍生角色资产ID和 prompt 的 @图N 一一对应。',
        },
      });
      await loadFlow();
      message.success(
        hasExisting ? '分镜面板已重新核对并修复' : '分镜面板已生成',
      );
    } finally {
      rebuildingStoryboardPanel.value = false;
    }
  }

  async function deleteStoryboard(row: any) {
    await removeStoryboard(row.id);
    await loadFlow();
  }

  const storyboardBusy = ref(false);
  const activeStoryboardNodeRunId = ref<number>();
  const lastStoryboardNodeRunId = ref<number>();
  const storyboardNodeRunState = ref('');
  const storyboardProgressCurrent = ref(0);
  const storyboardProgressTotal = ref(0);
  const workflowNodeRuns = reactive<Record<string, WorkflowNodeRun>>({});
  let storyboardPollTimer: ReturnType<typeof setTimeout> | undefined;
  let storyboardPollAttempts = 0;

  function scheduleStoryboardPolling() {
    if (!detailActive) return;
    if (storyboardPollTimer) clearTimeout(storyboardPollTimer);
    storyboardPollAttempts = 0;
    const refresh = async () => {
      const generatingIds = storyboards.value
        .filter((item) => item.state === '生成中')
        .map((item) => item.id);
      if (generatingIds.length === 0 && !activeStoryboardNodeRunId.value) {
        storyboardBusy.value = false;
        return;
      }
      if (storyboardPollAttempts >= 90) {
        storyboardBusy.value = false;
        message.warning('分镜生成仍在后台运行，请稍后刷新查看');
        return;
      }
      try {
        if (generatingIds.length > 0) {
          const completed = await pollStoryboardImages(generatingIds);
          const updates = new Map(completed.map((item) => [item.id, item]));
          storyboards.value = storyboards.value.map((item) =>
            updates.has(item.id) ? { ...item, ...updates.get(item.id) } : item,
          );
        }
        if (activeStoryboardNodeRunId.value) {
          const nodeRun = await getWorkflowNodeRun(
            activeStoryboardNodeRunId.value,
          );
          workflowNodeRuns.storyboard = nodeRun;
          storyboardNodeRunState.value = nodeRun.state;
          storyboardProgressCurrent.value = nodeRun.progressCurrent;
          storyboardProgressTotal.value = nodeRun.progressTotal;
          if (['cancelled', 'failed', 'success'].includes(nodeRun.state)) {
            lastStoryboardNodeRunId.value = nodeRun.id;
            activeStoryboardNodeRunId.value = undefined;
            storyboardBusy.value = false;
            await loadFlow();
            if (nodeRun.state === 'failed') {
              message.error(
                nodeRun.errorReason || '部分分镜图片生成失败，可点击重试失败项',
              );
            } else if (nodeRun.state === 'success') {
              message.success('分镜图片生成完成');
            }
          }
        }
      } finally {
        storyboardPollAttempts += 1;
        if (
          detailActive &&
          (storyboardBusy.value || activeStoryboardNodeRunId.value)
        ) {
          storyboardPollTimer = setTimeout(refresh, 2000);
        }
      }
    };
    storyboardPollTimer = setTimeout(refresh, 1200);
  }

  async function generateStoryboards(
    ids: number[],
    compulsory = false,
    concurrentCount = 2,
  ) {
    if (!selectedScriptId.value || ids.length === 0)
      return message.warning('请先选择需要生成的分镜');
    storyboardBusy.value = true;
    try {
      const flow = flowObject();
      await saveFlowData(projectId.value, selectedScriptId.value, flow);
      const workflowRun = await createWorkflowRun({
        projectId: projectId.value,
        scriptId: selectedScriptId.value,
        triggerType: 'manual',
        input: { requestedNodeId: 'storyboard' },
      });
      const nodeRun = await startWorkflowNode({
        workflowRunId: workflowRun.id,
        nodeId: 'storyboard',
        input: {
          storyboardIds: ids,
          concurrentCount: Math.max(1, Math.min(10, concurrentCount)),
          compulsory,
        },
      });
      activeStoryboardNodeRunId.value = nodeRun.id;
      lastStoryboardNodeRunId.value = nodeRun.id;
      storyboardNodeRunState.value = nodeRun.state;
      storyboardProgressCurrent.value = nodeRun.progressCurrent;
      storyboardProgressTotal.value = nodeRun.progressTotal;
      workflowNodeRuns.storyboard = await getWorkflowNodeRun(nodeRun.id);
      storyboards.value = storyboards.value.map((item) =>
        ids.includes(item.id)
          ? { ...item, reason: undefined, state: '生成中' }
          : item,
      );
      message.success(`已提交 ${nodeRun.progressTotal} 个分镜生成任务`);
      scheduleStoryboardPolling();
    } catch (error) {
      storyboardBusy.value = false;
      throw error;
    }
  }

  const workflowSequenceBusy = ref(false);
  let workflowRunPollTimer: ReturnType<typeof setTimeout> | undefined;
  let observedWorkflowRunId: number | undefined;

  function applyWorkflowRunNodes(nodes: WorkflowNodeRun[]) {
    for (const nodeRun of nodes) {
      workflowNodeRuns[nodeRun.nodeId] = nodeRun;
    }
    const storyboardRun = nodes.find((node) => node.nodeId === 'storyboard');
    if (storyboardRun) {
      storyboardNodeRunState.value = storyboardRun.state;
      storyboardProgressCurrent.value = storyboardRun.progressCurrent;
      storyboardProgressTotal.value = storyboardRun.progressTotal;
      storyboardBusy.value = storyboardRun.state === 'running';
      if (storyboardRun.state === 'running') {
        activeStoryboardNodeRunId.value = storyboardRun.id;
      } else {
        activeStoryboardNodeRunId.value = undefined;
      }
    }
  }

  function scheduleWorkflowRunPolling(workflowRunId: number) {
    if (!detailActive) return;
    if (observedWorkflowRunId === workflowRunId && workflowRunPollTimer) return;
    if (workflowRunPollTimer) clearTimeout(workflowRunPollTimer);
    observedWorkflowRunId = workflowRunId;
    const refresh = async () => {
      try {
        const run = await getWorkflowRun(workflowRunId);
        applyWorkflowRunNodes(run.nodes);
        if (['cancelled', 'failed', 'success'].includes(run.state)) {
          workflowRunPollTimer = undefined;
          observedWorkflowRunId = undefined;
          workflowSequenceBusy.value = false;
          await loadFlow();
          return;
        }
      } catch {
        // A transient request failure must not stop the backend workflow.
      }
      if (detailActive) workflowRunPollTimer = setTimeout(refresh, 1500);
    };
    workflowRunPollTimer = setTimeout(refresh, 500);
  }

  async function waitForWorkflowNode(nodeId: string, nodeRunId: number) {
    for (let attempt = 0; attempt < 600; attempt += 1) {
      if (!detailActive) return undefined;
      const nodeRun = await getWorkflowNodeRun(nodeRunId);
      workflowNodeRuns[nodeId] = nodeRun;
      if (['cancelled', 'failed', 'success'].includes(nodeRun.state))
        return nodeRun;
      await new Promise((resolve) => window.setTimeout(resolve, 1200));
    }
    throw new Error('节点运行超时，任务可能仍在后台执行');
  }

  async function waitForWorkflowRun(workflowRunId: number) {
    for (let attempt = 0; attempt < 1200; attempt += 1) {
      if (!detailActive) return undefined;
      const run = await getWorkflowRun(workflowRunId);
      applyWorkflowRunNodes(run.nodes);
      if (['cancelled', 'failed', 'success'].includes(run.state)) return run;
      await new Promise((resolve) => window.setTimeout(resolve, 1200));
    }
    throw new Error('前端等待超时，工作流仍会在后端继续运行');
  }

  async function runProductionWorkflowSequence(
    nodeIds: string[],
    overrides: Record<string, Record<string, unknown>> = {},
  ) {
    if (
      !selectedScriptId.value ||
      nodeIds.length === 0 ||
      workflowSequenceBusy.value
    )
      return;
    workflowSequenceBusy.value = true;
    try {
      const data = flowObject();
      const workflow = normalizeProductionWorkflow(data.workflow);
      const selectedIds = nodeIds.filter((id) =>
        workflow.nodes.some((node) => node.id === id),
      );
      await saveFlowData(projectId.value, selectedScriptId.value, data);
      const nodeInputs = Object.fromEntries(
        selectedIds.map((nodeId) => {
          const node = workflow.nodes.find((item) => item.id === nodeId)!;
          return [nodeId, { ...node.config, ...(overrides[nodeId] ?? {}) }];
        }),
      );
      const workflowRun = await createWorkflowRun({
        autoStart: true,
        projectId: projectId.value,
        scriptId: selectedScriptId.value,
        triggerType: selectedIds.length > 1 ? 'server-sequence' : 'server-node',
        input: { nodeInputs, requestedNodeIds: selectedIds },
      });
      const finished = await waitForWorkflowRun(workflowRun.id);
      if (!finished) return;
      if (finished.state === 'cancelled') {
        message.info('工作流已取消');
        await loadFlow();
        return;
      }
      if (finished.state !== 'success') {
        throw new Error(finished.errorReason || '工作流执行失败');
      }
      await loadFlow();
      message.success(
        selectedIds.length > 1 ? '工作流序列执行完成' : '节点执行完成',
      );
    } catch (error) {
      message.error(error instanceof Error ? error.message : '工作流执行失败');
    } finally {
      workflowSequenceBusy.value = false;
    }
  }

  async function runProductionWorkflowNode(
    nodeId: string,
    config: Record<string, unknown>,
  ) {
    await runProductionWorkflowSequence([nodeId], { [nodeId]: config });
  }

  async function cancelProductionWorkflowNode(nodeId: string) {
    const nodeRun = workflowNodeRuns[nodeId];
    if (!nodeRun || nodeRun.state !== 'running') return;
    await cancelWorkflowRun(nodeRun.workflowRunId);
    const run = await getWorkflowRun(nodeRun.workflowRunId);
    applyWorkflowRunNodes(run.nodes);
    if (nodeId === 'storyboard') {
      activeStoryboardNodeRunId.value = undefined;
      storyboardBusy.value = false;
    }
    message.success('工作流已取消');
  }

  async function retryProductionWorkflowNode(nodeId: string) {
    const source = workflowNodeRuns[nodeId];
    if (!source || !['cancelled', 'failed'].includes(source.state)) return;
    const started = await retryWorkflowNodeRun(source.id);
    workflowNodeRuns[nodeId] = await getWorkflowNodeRun(started.id);
    const finished = await waitForWorkflowNode(nodeId, started.id);
    if (!finished) return;
    await loadFlow();
    if (finished.state === 'success') message.success('节点重试成功');
    else message.error(finished.errorReason || '节点重试失败');
  }

  async function cancelStoryboardWorkflow() {
    if (!activeStoryboardNodeRunId.value) return;
    await cancelWorkflowNodeRun(activeStoryboardNodeRunId.value);
    if (storyboardPollTimer) clearTimeout(storyboardPollTimer);
    storyboards.value = storyboards.value.map((item) =>
      item.state === '生成中'
        ? { ...item, reason: '用户取消生成', state: '已取消' }
        : item,
    );
    lastStoryboardNodeRunId.value = activeStoryboardNodeRunId.value;
    activeStoryboardNodeRunId.value = undefined;
    storyboardNodeRunState.value = 'cancelled';
    workflowNodeRuns.storyboard = await getWorkflowNodeRun(
      lastStoryboardNodeRunId.value,
    );
    storyboardBusy.value = false;
    message.success('已取消分镜图片生成');
  }

  async function retryStoryboardWorkflow() {
    if (!lastStoryboardNodeRunId.value) return;
    const retryable = storyboards.value.filter((item) =>
      ['已取消', '生成失败'].includes(item.state || ''),
    );
    if (retryable.length === 0) {
      storyboardNodeRunState.value = '';
      return message.info(
        '当前没有生成失败或已取消的分镜，请勾选“未生成”分镜后点击生成选中',
      );
    }
    storyboardBusy.value = true;
    try {
      const nodeRun = await retryWorkflowNodeRun(lastStoryboardNodeRunId.value);
      activeStoryboardNodeRunId.value = nodeRun.id;
      lastStoryboardNodeRunId.value = nodeRun.id;
      storyboardNodeRunState.value = nodeRun.state;
      storyboardProgressCurrent.value = nodeRun.progressCurrent;
      storyboardProgressTotal.value = nodeRun.progressTotal;
      workflowNodeRuns.storyboard = await getWorkflowNodeRun(nodeRun.id);
      storyboards.value = storyboards.value.map((item) =>
        ['已取消', '生成失败'].includes(item.state || '')
          ? { ...item, reason: undefined, state: '生成中' }
          : item,
      );
      message.success(`正在重试 ${nodeRun.progressTotal} 个失败分镜`);
      scheduleStoryboardPolling();
    } catch (error) {
      storyboardBusy.value = false;
      throw error;
    }
  }

  async function batchDeleteSelectedStoryboards(ids: number[]) {
    await batchDeleteStoryboards(ids, projectId.value);
    message.success(`已删除 ${ids.length} 个分镜`);
    await loadFlow();
  }

  async function previewAllStoryboardImages() {
    if (storyboards.value.length === 0) return message.warning('当前没有分镜');
    storyboardPreview.value = assetFileUrl(
      (await previewStoryboardImages(
        storyboards.value.map((item) => item.id),
      )) || '',
    );
    if (!storyboardPreview.value)
      return message.warning('还没有可预览的分镜图片');
    storyboardPreviewOpen.value = true;
  }

  async function downloadAllStoryboardImages() {
    const ids = storyboards.value.map((item) => item.id);
    if (ids.length === 0) return message.warning('当前没有分镜');
    const blob = await downloadStoryboardPreview(ids);
    downloadFileFromBlob({
      fileName: `storyboard-${selectedScriptId.value ?? 'preview'}.png`,
      source: blob,
    });
  }

  async function downloadSelectedStoryboardImages(ids: number[]) {
    if (ids.length === 0) return message.warning('请至少选择一个分镜');
    const blob = await downloadStoryboardPreview(ids);
    downloadFileFromBlob({
      fileName: `storyboard-selected-${selectedScriptId.value ?? 'preview'}.png`,
      source: blob,
    });
  }

  async function saveStoryboardOrder(ids: number[]) {
    if (!selectedScriptId.value) return;
    await reorderStoryboards(projectId.value, selectedScriptId.value, ids);
    const order = new Map(ids.map((id, index) => [id, index]));
    storyboards.value = [...storyboards.value]
      .sort((a, b) => (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0))
      .map((item, index) => ({ ...item, index }));
    message.success('分镜顺序已保存');
  }

  async function openVideoTrack(trackId: number) {
    const track = videoTracks.value.find((item: any) => item.id === trackId);
    if (!track) return message.warning('未找到对应的视频轨道');
    productionFlowCanvasRef.value?.focusNode('workbench');
    openTrackBinding(track);
  }
  function openTrackBinding(track: any) {
    trackBindingTarget.value = track;
    trackBindingStoryboardIds.value = [
      ...new Set<number>(
        (track.medias ?? [])
          .filter((media: any) => media.sources === 'storyboard')
          .map((media: any) => Number(media.id)),
      ),
    ];
    trackBindingOpen.value = true;
  }
  async function confirmTrackBinding() {
    const track = trackBindingTarget.value;
    if (!track) return;
    const storyboardIds = [...new Set<number>(trackBindingStoryboardIds.value)];
    await bindTrackStoryboards(track.id, storyboardIds);
    trackBindingOpen.value = false;
    await loadFlow();
    message.success('分镜已移入该轨道');
  }
  async function saveTrackPrompt(track: any) {
    await updateVideoTrackPrompt(track.id, track.prompt || '');
    message.success('视频提示词已保存');
  }
  async function createVideoPrompt(track: any) {
    if (!project.value?.videoModel) return message.warning('请先配置视频模型');
    track.promptGenerating = true;
    try {
      track.prompt = await generateVideoPrompt({
        trackId: track.id,
        projectId: projectId.value,
        info: track.medias ?? [],
        model: project.value.videoModel,
        mode: videoModeForTrack(track),
      });
      message.success('视频提示词已生成');
    } finally {
      track.promptGenerating = false;
    }
  }
  let videoPollTimer: ReturnType<typeof setTimeout> | undefined;
  function startVideoPolling() {
    if (!detailActive) return;
    if (videoPollTimer) clearTimeout(videoPollTimer);
    const refresh = async () => {
      const project = Number(projectId.value);
      const script = Number(selectedScriptId.value);
      if (
        !Number.isSafeInteger(project) ||
        project <= 0 ||
        !Number.isSafeInteger(script) ||
        script <= 0
      ) {
        videoPollTimer = undefined;
        return;
      }
      const generating = [
        ...new Set(
          videoTracks.value.flatMap((track: any) =>
            (track.videoList ?? [])
              .filter((video: any) => video.state === '生成中')
              .map((video: any) => Number(video.id))
              .filter((id: number) => Number.isSafeInteger(id) && id > 0),
          ),
        ),
      ];
      if (!generating.length) {
        videoTracks.value.forEach((track: any) => {
          track.videoGenerating = false;
        });
        videoPollTimer = undefined;
        return;
      }
      try {
        const updates = await pollTrackVideos(project, script, generating);
        for (const video of updates) {
          if (video.state === '生成成功') {
            message.success({
              content: `视频任务 ${video.id} 生成完成`,
              key: `video-${video.id}`,
            });
          } else if (video.state === '生成失败') {
            message.error({
              content: video.errorReason || `视频任务 ${video.id} 生成失败`,
              duration: 6,
              key: `video-${video.id}`,
            });
          }
        }
        const updateMap = new Map(updates.map((video) => [video.id, video]));
        videoTracks.value = videoTracks.value.map((track: any) => ({
          ...track,
          videoGenerating: (track.videoList ?? []).some(
            (video: any) =>
              (updateMap.get(video.id)?.state ?? video.state) === '生成中',
          ),
          videoList: (track.videoList ?? []).map((video: any) => ({
            ...video,
            ...(updateMap.get(video.id) ?? {}),
          })),
        }));
      } catch (error: any) {
        const status = Number(
          error?.code ?? error?.status ?? error?.response?.status,
        );
        if (status === 422) {
          videoPollTimer = undefined;
          return;
        }
        // Keep polling; transient status failures should not hide the running task.
      }
      if (detailActive) videoPollTimer = setTimeout(refresh, 2000);
    };
    videoPollTimer = setTimeout(refresh, 800);
  }
  async function generateVideo(track: any) {
    if (!selectedScriptId.value || !project.value?.videoModel)
      return message.warning('请先配置项目视频模型');
    if (!ensureVideoStoryboardImagesCurrent([track])) return;
    const options = track.generation ?? {};
    track.videoGenerating = true;
    const mode = options.mode || videoModeForTrack(track);
    const storyboard = storyboardForTrack(track);
    let id: number;
    try {
      id = await generateTrackVideo({
        projectId: projectId.value,
        scriptId: selectedScriptId.value,
        trackId: track.id,
        prompt: track.prompt || storyboard?.videoDesc || '',
        model: options.model || project.value.videoModel,
        mode,
        continuityMode:
          options.continuityMode || track.continuityMode || 'auto',
        resolution: options.resolution || '1080p',
        duration: track.duration || storyboard?.duration || 4,
        audio: options.audio !== false,
        uploadData: videoUploadData(track, mode),
      });
    } catch (error) {
      track.videoGenerating = false;
      throw error;
    }
    track.videoList = [
      { id, state: '生成中', src: '', errorReason: undefined },
      ...(track.videoList ?? []),
    ];
    message.loading({
      content: `轨道 ${track.id} 正在生成视频`,
      duration: 2,
      key: `video-${id}`,
    });
    startVideoPolling();
  }
  async function chooseVideo(track: any, video: any) {
    await selectTrackVideo(track.id, video.id);
    track.selectVideoId = video.id;
    message.success('候选视频已选择');
  }
  async function removeVideo(video: any) {
    await deleteTrackVideo(video.id);
    await loadFlow();
  }
  async function cancelVideo(video: any) {
    await cancelTrackVideo(video.id);
    await loadFlow();
  }
  async function retryVideo(video: any, track: any) {
    if (!project.value?.videoModel) return message.warning('请先配置视频模型');
    if (!ensureVideoStoryboardImagesCurrent([track])) return;
    const mode = videoModeForTrack(track);
    const id = await retryTrackVideo({
      id: video.id,
      model: project.value.videoModel,
      mode,
      resolution: '1080p',
      audio: true,
      uploadData: videoUploadData(track, mode),
    });
    track.videoList = [
      { id, state: '生成中', src: '', errorReason: undefined },
      ...(track.videoList ?? []),
    ];
    message.loading({
      content: `视频任务 ${id} 正在重试`,
      duration: 2,
      key: `video-${id}`,
    });
    startVideoPolling();
  }
  async function exportVideo(videoIds: number[] = []) {
    if (!selectedScriptId.value) return message.warning('请先选择剧本');
    if (videoIds.length < 2) return message.warning('请至少选择 2 个视频片段');
    const result = await exportFinalVideo(
      projectId.value,
      selectedScriptId.value,
      videoIds,
    );
    message.success(
      `已提交 ${videoIds.length} 个视频片段的合成任务 ${result.taskId}，完成后会自动归档到项目“剧集成果”`,
    );
  }

  function openProductionForScript(scriptId: number) {
    changeProductionScript(scriptId, (scriptChanged) => {
      activeTab.value = 'production';
      if (!scriptChanged) void loadFlow();
    });
  }

  const panelContext = reactive({
    projectId,
    project,
    imageQuality,
    videoMode,
    novels,
    novelColumns,
    novelLoading,
    productionNodeId,
    rememberProductionNode,
    novelPage,
    novelPageSize,
    novelTotal,
    changeNovelPage,
    importNovelFile,
    extractSelectedNovelEvents,
    formatEventDisplay,
    extractNovelEvents,
    removeNovel,
    reloadNovels: loadNovels,
    scriptChatMessages,
    onAgentToolResult,
    resetAgentWorkspace,
    openScriptGeneration,
    clearScriptAgentMemory,
    saveAgentWorkspace,
    workspaceActiveTab,
    workspaceTabs,
    renderMarkdown,
    assets,
    visibleScripts,
    scriptAssetGroups,
    scriptSearch,
    selectedScriptIds,
    importScriptFiles,
    toggleAllScripts,
    batchExportScripts,
    batchExtractScriptAssets,
    batchRemoveScripts,
    toggleScript,
    extractAssetsFromScript,
    removeScript,
    scriptExtractLabel,
    scriptExtractColor,
    selectedScriptId,
    loadedFlowProjectId,
    loadedFlowScriptId,
    scriptOptions,
    productionAssets,
    flowText,
    selectedScript,
    storyboards,
    storyboardBusy,
    storyboardProgressCurrent,
    storyboardProgressTotal,
    storyboardNodeRunState,
    videoTracks,
    workflowNodeRuns,
    rebuildingStoryboardPanel,
    productionAgentCollapsed,
    productionAgentActivity,
    productionChatMessages,
    trackBindingOpen,
    trackBindingTarget,
    trackBindingStoryboardIds,
    changeProductionScript,
    loadFlow,
    saveFlowText,
    rebuildStoryboardPanel,
    previewAllStoryboardImages,
    cancelProductionWorkflowNode,
    cancelVideo,
    cancelStoryboardWorkflow,
    removeVideo,
    batchDeleteSelectedStoryboards,
    generateSelectedVideoPrompts,
    generateSelectedVideos,
    downloadSelectedVideos,
    openAssetImageFlow,
    openStoryboardImageFlow,
    downloadSelectedStoryboardImages,
    exportVideo,
    generateStoryboards,
    generateDerivedAsset,
    generateVideo,
    createVideoPrompt,
    openVideoTrack,
    retryVideo,
    retryStoryboardWorkflow,
    retryProductionWorkflowNode,
    runProductionWorkflowNode,
    runProductionWorkflowSequence,
    deleteStoryboard,
    saveStoryboardOrder,
    saveProductionCanvasPositions,
    saveProductionWorkflow,
    saveTrackPrompt,
    chooseVideo,
    updateProductionFlowSection,
    resetProductionAgent,
    onProductionAgentActivity,
    clearProductionAgentMemory,
    previewProductionFlowSection,
    confirmTrackBinding,
    openProductionForScript,
    openAssets: () =>
      router.push({
        path: '/toonflow/assets',
        query: { projectId: projectId.value },
      }),
    chooseScriptFiles: () => scriptUploadInput.value?.click(),
    setScriptUploadInput: (element: HTMLInputElement | null) => {
      scriptUploadInput.value = element ?? undefined;
    },
    setAgentChatRef: (instance: InstanceType<typeof AgentChat> | null) => {
      agentChatRef.value = instance;
    },
    setProductionAgentChatRef: (
      instance: InstanceType<typeof AgentChat> | null,
    ) => {
      productionAgentChatRef.value = instance;
    },
    setProductionFlowCanvasRef: (
      instance: InstanceType<typeof ProductionFlowCanvas> | null,
    ) => {
      productionFlowCanvasRef.value = instance;
    },
    focusProductionNode: (nodeId: string) =>
      productionFlowCanvasRef.value?.focusNode(nodeId),
    assetOptions,
    reloadScriptsAndFlow: async () => {
      await loadScripts();
      await loadFlow();
    },
    reloadFlow: loadFlow,
    flowImageModalOpen,
    editingAssetName,
    imageFlowEditorKey,
    imageFlowNodes,
    imageFlowEdges,
    imageFlowAssetOptions,
    generatingImageNodeId,
    addImageFlowNode,
    createFlowImage,
    removeImageFlowNode,
    connectImageFlow,
    selectImageFlowAsset,
    uploadImageFlowReference,
    planImageEditWithAgent,
    saveVisualImageFlow,
    storyboardPreviewOpen,
    storyboardPreview,
    downloadAllStoryboardImages,
  });

  watch(selectedScriptId, () => {
    productionNodeId.value = selectedScriptId.value
      ? savedLocation.nodes[String(selectedScriptId.value)]
      : undefined;
    persistLocation();
    // The selected episode changes synchronously while its workspace loads
    // asynchronously. Keep consumers from parsing the previous episode's plan
    // or assets during that gap.
    if (loadedFlowScriptId.value !== selectedScriptId.value) {
      loadedFlowProjectId.value = undefined;
      loadedFlowScriptId.value = undefined;
    }
    if (workflowRunPollTimer) clearTimeout(workflowRunPollTimer);
    workflowRunPollTimer = undefined;
    observedWorkflowRunId = undefined;
    stopProductionAgentSync();
    productionAgentActivity.value = '等待指令';
    productionChatMessages.value = [];
    if (!refreshingAll) void loadFlow();
  });

  watch(activeTab, (tab) => {
    persistLocation();
    if (tab === 'script-agent') agentType.value = 'scriptAgent';
    if (tab === 'production') agentType.value = 'productionAgent';
    if (tab !== 'production') {
      productionAgentCollapsed.value = true;
      stopProductionAgentSync();
    }
    loadAgentMemory();
  });

  watch(productionAgentCollapsed, (collapsed) => {
    if (collapsed) stopProductionAgentSync();
  });

  onMounted(() => {
    detailActive = true;
    void loadAll();
  });
  onBeforeUnmount(() => {
    persistLocation();
    detailActive = false;
    loadFlowQueued = false;
    novelLoadVersion += 1;
    if (workflowRunPollTimer) clearTimeout(workflowRunPollTimer);
    if (productionAssetRefreshTimer) clearTimeout(productionAssetRefreshTimer);
    if (storyboardPollTimer) clearTimeout(storyboardPollTimer);
    if (videoPollTimer) clearTimeout(videoPollTimer);
    stopProductionAgentSync();
  });
  watch(projectId, () => {
    locationReady = false;
    savedLocation = locationStore.read(projectId.value);
    activeTab.value = savedLocation.stage;
    selectedScriptId.value = savedLocation.scriptId;
    loadedFlowProjectId.value = undefined;
    loadedFlowScriptId.value = undefined;
    void loadAll();
  });

  return reactive({
    loadError,
    activeTab,
    stages,
    activePanelComponent,
    loading,
    project,
    statistics,
    router,
    loadAll,
    panelContext,
  });
}
