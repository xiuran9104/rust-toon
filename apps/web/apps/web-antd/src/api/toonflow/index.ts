import { requestClient } from '#/api/request';

export namespace ToonflowApi {
  export interface Project {
    id: number;
    projectType: string;
    chatModel?: number;
    imageModel?: number;
    imageQuality: string;
    videoModel?: number;
    name: string;
    intro: string;
    type: string;
    artStyle: string;
    directorManual: string;
    mode: string;
    videoRatio: string;
    createTime: number;
    updateTime: number;
  }

  export interface SaveProject {
    id?: number;
    projectType: string;
    chatModel?: number;
    imageModel?: number;
    imageQuality: string;
    videoModel?: number;
    name: string;
    intro: string;
    type: string;
    artStyle: string;
    directorManual: string;
    mode: string;
    videoRatio: string;
  }

  export interface NovelChapter {
    id: number;
    index: number;
    reel: string;
    chapter: string;
    chapterData: string;
    projectId: number;
    eventState: number;
    event?: string;
    errorReason?: string;
    createTime: number;
  }

  export interface NovelPage {
    data: NovelChapter[];
    total: number;
  }

  export interface Script {
    id: number;
    name: string;
    content: string;
    projectId: number;
    extractState?: number;
    errorReason?: string;
    createTime: number;
    relatedAssets: Array<{ id: number; name: string }>;
  }

  export interface Asset {
    derive?: Asset[];
    appearances?: Array<{
      id: number;
      roleAssetId: number;
      name: string;
      scenes: string[];
      costumePrompt: string;
      description: string;
    }>;
    id: number;
    name: string;
    prompt: string;
    remark?: string;
    type: string;
    description: string;
    /** Legacy Toonflow clients used `describe`; keep both during migration. */
    describe?: string;
    scriptId?: number;
    imageId?: number;
    imageFilePath?: string;
    imageUrl?: string;
    audioUrl?: string;
    imageState?: string;
    imageErrorReason?: string;
    parentAssetId?: number;
    appearanceId?: number;
    projectId: number;
    flowId?: number;
    audioBindState?: number;
  }

  export interface LibraryAsset extends Asset {
    linkedToProject: boolean;
    sourceProjectName: string;
  }

  export interface Storyboard {
    id: number;
    scriptId: number;
    projectId: number;
    prompt: string;
    duration?: number;
    state?: string;
    track?: string;
    videoDesc?: string;
    shotSize?: string;
    cameraMove?: string;
    timeOfDay?: string;
    carriedObjects?: Array<{ description?: string; era?: string; name: string }>;
    shouldGenerateImage: number | boolean;
    associateAssetsIds: number[];
    describe?: string;
    filePath?: string;
    flowId?: number;
    index?: number;
    reason?: string;
    sceneKey?: string;
    sceneMasterId?: number;
    sceneMasterName?: string;
    sceneMasterStatus?: 'missing_reference' | 'needs_review' | 'ready';
    sceneStateId?: number;
    sceneStateKey?: string;
    sceneStateParentKey?: string;
    sceneStateDescription?: string;
    sceneStateName?: string;
    generatedSceneStateId?: number;
    sceneConsistencyStatus?:
      | 'missing_master'
      | 'missing_scene_key'
      | 'ready'
      | 'stale'
      | 'unconfigured';
    src?: string;
    trackId?: number;
  }

  export interface SceneStateReference {
    assetId: number;
    assetName: string;
    imageId: number;
    promptLabel: string;
    referenceUrl: string;
    role: 'master' | 'object_detail' | 'state';
    sortOrder: number;
  }

  export interface SceneState {
    changeSummary: string;
    createTime: number;
    id: number;
    name: string;
    objectStates: Record<string, string>;
    parentStateId?: number;
    references: SceneStateReference[];
    revision: number;
    sceneMasterId: number;
    sequence: number;
    source: string;
    stateKey: string;
    statePrompt: string;
    storyboardCount: number;
    updateTime: number;
  }

  export interface SceneMaster {
    createTime: number;
    id: number;
    layoutSpec: Record<string, unknown>;
    name: string;
    pinnedImageId?: number;
    projectId: number;
    referenceUrl?: string;
    revision: number;
    sceneAssetId?: number;
    sceneAssetName?: string;
    sceneKey: string;
    scriptId: number;
    source: string;
    spatialPrompt: string;
    states: SceneState[];
    status: 'missing_reference' | 'needs_review' | 'ready';
    updateTime: number;
  }

  export interface SceneConsistencyCatalog {
    scenes: SceneMaster[];
  }

  export interface AutoSceneConsistencyResult {
    sceneCount: number;
    statesCreated: number;
    storyboardsBound: number;
    warnings: string[];
  }

  export type VideoTransitionType =
    | 'action_bridge'
    | 'audio_bridge'
    | 'continuous'
    | 'cut'
    | 'dissolve'
    | 'empty_shot'
    | 'match_cut';

  export type VideoFramePolicy = 'own' | 'previous_tail';

  export interface UpdateVideoTransitionSettings {
    framePolicy: VideoFramePolicy;
    id: number;
    previousTrackId?: number;
    transitionDurationMs?: number;
    transitionType: VideoTransitionType;
    trimEndMs?: null | number;
    trimStartMs?: number;
  }

  export interface AgentDeployment {
    id: number;
    key: string;
    description: string;
    name: string;
    temperature: number;
    maxOutputTokens: number;
    disabled: boolean;
    modelConfigId?: number;
    modelType: 'chat' | 'image' | 'speech' | 'video';
    promptSourceKey?: string;
    memoryScope?: string;
  }

  export interface ArtStyle { id: number; name: string; fileUrl: string; label: string; prompt: string; createTime: number }
  export interface Task { id: string | number; projectId?: number; projectName?: string; taskClass: string; relatedObjects: string; model: string; description: string; state: string; startTime?: number; reason?: string; input?: Record<string, any>; retryOfId?: string | number; progressCurrent?: number; progressTotal?: number }
  export interface Prompt { id: number; name: string; type: string; data: string; useData?: string; sourceKey?: string }
  export interface Skill { id: string; name: string; description: string; type: string; path: string; state: number; createTime: number; updateTime: number }
  export interface ProjectStatistics { roleCount: number; scriptCount: number; videoCount: number; storyboardCount: number }
  export interface EpisodeRender {
    id: number;
    projectId: number;
    scriptId: number;
    url: string;
    objectPath: string;
    filePath: string;
    posterUrl?: null | string;
    state: string;
    isCurrent: boolean;
    version: number;
    sourceVideoIds: number[];
    metadata: unknown;
    createdAt: number | string;
    updatedAt: number | string;
    createdBy: string;
    exportTaskId?: null | number;
  }
  export interface VideoArchiveEpisode {
    scriptId: number;
    scriptName: string;
    episodeNo?: null | number;
    renders: EpisodeRender[];
  }
  export interface ProjectVideoArchive {
    projectId: number;
    episodes: VideoArchiveEpisode[];
  }
  export interface EpisodeRenderList {
    projectId: number;
    scriptId: number;
    renders: EpisodeRender[];
  }
  export interface CreativeManual { id: number; kind: 'director' | 'visual'; name: string; path: string; images: string[]; data: Array<{ label: string; value: string; data: string }>; createTime: number; updateTime: number }
  export interface AgentMemory { id: number; role: string; content: string; memoryType: 'message' | 'summary'; createTime: number }
  export interface AgentRun { id: number; agentType: string; isolationKey: string; projectId: number; scriptId?: number; input: string; output?: string; state: string; errorReason?: string; startTime: number; finishTime?: number; retryOfId?: number }
  export interface AgentRunEvent { id: number; runId: number; eventType: string; data: { text?: string }; createTime: number }
}

export const getArtStyles = () => requestClient.get<ToonflowApi.ArtStyle[]>('/toonflow/art-styles');
export const saveArtStyle = (data: Partial<ToonflowApi.ArtStyle> & { name: string }) => requestClient.post('/toonflow/art-styles', data);
export const deleteArtStyle = (id: number) => requestClient.delete(`/toonflow/art-styles/${id}`);
export const getTasks = () => requestClient.get<ToonflowApi.Task[]>('/toonflow/tasks');
export interface TaskQuery {
  page: number;
  limit: number;
  taskClass?: string;
  state?: string;
  projectId?: number;
}
export interface TaskStats { total: number; running: number; success: number; failed: number }
export const getTaskPage = (query: TaskQuery) => requestClient.post<{
  data: ToonflowApi.Task[]; total: number; stats: TaskStats;
}>('/task/getTaskApi', query);
export const getTaskDetails = (taskId: string | number) => requestClient.post<ToonflowApi.Task | null>('/task/taskDetails', { taskId });
export const getTaskCategories = () => requestClient.post<Array<{ taskClass: string }>>('/task/getTaskCategories');
export const getTaskProjects = () => requestClient.post<Array<{ id: number; name: string }>>('/task/getProject');
export const getPrompts = () => requestClient.get<ToonflowApi.Prompt[]>('/toonflow/prompts');
export const savePrompt = (data: Partial<ToonflowApi.Prompt> & { name: string; type: string }) => requestClient.post('/toonflow/prompts', data);
export const deletePrompt = (id: number) => requestClient.delete(`/toonflow/prompts/${id}`);
export const getSkills = () => requestClient.get<ToonflowApi.Skill[]>('/toonflow/skills');
export const getSkillContent = (path: string) => requestClient.post<string>('/setting/skillManagement/getSkillContent', { path });
export const saveSkillContent = (path: string, content: string) => requestClient.post('/setting/skillManagement/saveSkillContent', { path, content });
export const getProjectStatistics = (projectId: number) => requestClient.post<ToonflowApi.ProjectStatistics>('/general/generalStatistics', { projectId });
export const updateProjectProfile = (data: { id: number; intro?: string; type?: string; artStyle?: string; videoRatio?: string; projectType?: string }) => requestClient.post('/general/updateProject', data);
export const getAgentModelDetails = (key: 'productionAgent' | 'scriptAgent') => requestClient.post<Record<string, any>>('/project/getModelDetails', { key });
export const generateNovelEvents = (projectId: number, novelIds: number[]) => requestClient.post('/novel/event/generateEvents', { projectId, novelIds, concurrentCount: 5 });
export const extractScriptAssets = (projectId: number, scriptIds: number[]) => requestClient.post<{ taskId: number }>('/script/extractAssets', { projectId, scriptIds, groupSize: 5 });
export const pollScriptAssets = (ids: number[]) => requestClient.post<Array<{ id: number; extractState: number; errorReason?: string; appearanceCount: number }>>('/script/pollScriptAssets', { ids });
export const polishAssetPrompt = (data: { assetsId: number; projectId: number; type: string; name: string; describe: string }) => requestClient.post<{ prompt: string; assetsId: number }>('/assetsGenerate/polishAssetsPrompt', data);
export const generateAssetImage = (data: { projectId: number; model: number | string; resolution: string; id: number; type: string; name: string; prompt: string; base64?: string }) => requestClient.post<{ path: string; assetsId: number }>('/assetsGenerate/generateAssets', data);
export const queueAssetImages = (data: { projectId: number; model: number | string; resolution: string; concurrentCount?: number; items: Array<{ id: number; type: string; name: string; prompt: string; base64?: string }> }) => requestClient.post<{ total: number }>('/assetsGenerate/batchGenerateImageAssets', data);
export const retryAssetImages = (data: { projectId: number; ids: number[]; concurrentCount?: number }) => requestClient.post<{ total: number; ids: number[] }>('/assetsGenerate/retryImageAssets', data);
export const pollAssetImages = (ids: number[]) => requestClient.post<Array<{ id: number; state: string; filePath?: string; errorReason?: string; imageId: number }>>('/assets/pollingImageAssets', { ids });
export const batchGenerateAssetsImage = queueAssetImages;
export const pollingImage = pollAssetImages;
export const cancelAssetImage = (id: number) => requestClient.post('/assetsGenerate/cancelGenerate', { id });
export const uploadMaterial = (data: { projectId: number; base64Data: string; type?: string; name: string }) => requestClient.post('/assets/uploadClip', data);
export const getMaterialData = (projectId: number, scriptId?: number) => requestClient.post<{ data: any[]; video: any[] }>('/assets/getMaterialData', { projectId, scriptId });
export const generateFlowImage = (data: { projectId: number; storyboardId?: number; model: number | string; quality: string; ratio: string; prompt: string; references?: string[]; targetType?: 'costume' | 'role' | 'scene' | 'storyboard' | 'tool' }) => requestClient.post<{ sceneGenerationContext?: Record<string, unknown>; sceneStateId?: number; url: string }>('/production/editImage/generateFlowImage', data);
export const getImageFlow = (id: number) => requestClient.post<{ id: number; nodes: any[]; edges: any[] } | null>('/production/editImage/getImageFlow', { id });
export const saveImageFlow = (nodes: any[], edges: any[], assetId?: number) => requestClient.post<{ id: number }>('/production/editImage/saveImageFlow', { nodes, edges, assetId });
export const updateImageFlow = (flowId: number, nodes: any[], edges: any[]) => requestClient.post('/production/editImage/updateImageFlow', { flowId, nodes, edges });
export const uploadFlowImage = (projectId: number, scriptId: number, base64Data: string) => requestClient.post<string>('/production/editImage/uploadImage', { projectId, scriptId, base64Data });
export const generateStoryboardImages = (data: { storyboardIds: number[]; projectId: number; scriptId: number; concurrentCount?: number; compulsory?: boolean }) => requestClient.post('/production/storyboard/batchGenerateImage', data);
export const pollStoryboardImages = (ids: number[]) => requestClient.post<Array<Pick<ToonflowApi.Storyboard, 'filePath' | 'id' | 'prompt' | 'reason' | 'src' | 'state'>>>('/production/storyboard/pollingImage', { ids });
export const batchDeleteStoryboards = (ids: number[], projectId: number) => requestClient.post('/production/storyboard/batchDelete', { ids, projectId });
export const previewStoryboardImages = (storyboardIds: number[]) => requestClient.post<string | null>('/production/storyboard/previewImage', { storyboardIds });
export const downloadStoryboardPreview = (storyboardIds: number[]) => requestClient.download<Blob>('/production/storyboard/downPreviewImage', { method: 'POST', data: { storyboardIds } });
export const updateStoryboardUrl = (id: number, url: string, flowId: number, generatedSceneStateId?: number, sceneGenerationContext?: Record<string, unknown>) => requestClient.post('/production/storyboard/updateStoryboardUrl', { id, url, flowId, generatedSceneStateId, sceneGenerationContext });
export const getCreativeManuals = () => requestClient.get<ToonflowApi.CreativeManual[]>('/toonflow/manuals');
export const saveCreativeManual = (manual: Partial<ToonflowApi.CreativeManual> & { kind: 'director' | 'visual'; name: string; path: string }) => requestClient.post(`/project/${manual.kind === 'visual' ? (manual.id ? 'editVisualManual' : 'addVisualManual') : (manual.id ? 'editDirectorlManual' : 'addDirectorManual')}`, { name: manual.name, images: manual.images ?? [], data: manual.data ?? [], ...(manual.kind === 'visual' ? { stylePath: manual.path } : { directorManual: manual.path }) });
export const deleteCreativeManual = (kind: 'director' | 'visual', name: string) => requestClient.post(`/project/${kind === 'visual' ? 'deleteVisualManual' : 'deleteDirectorManual'}`, { name });

export function getProjects() {
  return requestClient.get<ToonflowApi.Project[]>('/toonflow/projects');
}

export function getProject(id: number) {
  return requestClient.get<ToonflowApi.Project>(`/toonflow/projects/${id}`);
}

export function getProjectVideoArchive(projectId: number) {
  return requestClient.get<ToonflowApi.ProjectVideoArchive>(
    `/toonflow/projects/${projectId}/video-archive`,
  );
}

export function getEpisodeRenders(projectId: number, scriptId: number) {
  return requestClient.get<ToonflowApi.EpisodeRenderList>(
    `/toonflow/projects/${projectId}/episodes/${scriptId}/renders`,
  );
}

export function setEpisodeRenderCurrent(id: number) {
  return requestClient.request<ToonflowApi.EpisodeRender>(
    `/toonflow/episode-renders/${id}/current`,
    { data: {}, method: 'PATCH' },
  );
}

export function createProject(data: ToonflowApi.SaveProject) {
  return requestClient.post<{ id: number }>('/toonflow/project/addProject', data);
}

export function updateProject(data: ToonflowApi.SaveProject) {
  return requestClient.post('/toonflow/project/editProject', data);
}

export function deleteProject(id: number) {
  return requestClient.post('/toonflow/project/delProject', { id });
}

export function addNovel(projectId: number, data: Array<Partial<ToonflowApi.NovelChapter>>) {
  return requestClient.post('/toonflow/novel/addNovel', { projectId, data });
}

export function getNovelData(projectId: number) {
  if (!Number.isSafeInteger(projectId) || projectId <= 0) {
    return Promise.resolve([] as ToonflowApi.NovelChapter[]);
  }
  return requestClient.post<ToonflowApi.NovelChapter[]>('/toonflow/novel/getNovelData', {
    projectId,
  });
}

export function getNovelPage(
  projectId: number,
  page = 1,
  limit = 10,
  search?: string,
) {
  if (!Number.isSafeInteger(projectId) || projectId <= 0) {
    return Promise.resolve({ data: [], total: 0 } as ToonflowApi.NovelPage);
  }
  return requestClient.post<ToonflowApi.NovelPage>(
    '/toonflow/novel/getNovel',
    {
      limit,
      page,
      projectId,
      ...(search ? { search } : {}),
    },
  );
}

export function updateNovel(data: Partial<ToonflowApi.NovelChapter> & { id: number }) {
  return requestClient.post('/toonflow/novel/updateNovel', data);
}

export function deleteNovel(id: number) {
  return requestClient.post('/toonflow/novel/delNovel', { id });
}

export function getScripts(projectId: number, name = '') {
  return requestClient.post<ToonflowApi.Script[]>('/toonflow/script/getScrptApi', {
    projectId,
    name,
  });
}

export function addScript(data: {
  assets: number[];
  content: string;
  name: string;
  projectId: number;
}) {
  return requestClient.post<{ id: number }>('/toonflow/script/addScript', data);
}

export function updateScript(data: {
  assets: number[];
  content: string;
  id: number;
  name: string;
}) {
  return requestClient.post('/toonflow/script/updateScript', data);
}

export function deleteScripts(ids: number[]) {
  return requestClient.post('/toonflow/script/delScript', { ids });
}

export function exportScripts(ids: number[]) {
  return requestClient.download<Blob>('/script/exportScript', {
    method: 'POST',
    data: { id: ids },
  });
}

export function getAssets(projectId: number) {
  return requestClient.post<ToonflowApi.Asset[]>('/toonflow/assets/getAssetsApi', {
    projectId,
  });
}

export function getAssetLibrary(projectId: number) {
  return requestClient.post<ToonflowApi.LibraryAsset[]>('/toonflow/assets/library', {
    projectId,
  });
}

export function saveAsset(data: Partial<ToonflowApi.Asset> & { name: string; projectId: number }) {
  return requestClient.post<{ id: number }>('/toonflow/assets/saveAssets', data);
}

export function deleteAssets(ids: number[]) {
  return requestClient.post('/toonflow/assets/batchDelete', { ids });
}

export function getFlowData(projectId: number, episodesId: number) {
  return requestClient.post<Record<string, any>>('/toonflow/production/getFlowData', {
    projectId,
    episodesId,
  });
}

export function saveFlowData(projectId: number, episodesId: number, data: Record<string, any>) {
  return requestClient.post('/toonflow/production/saveFlowData', {
    projectId,
    episodesId,
    data,
  });
}

export function validateWorkflow(workflow: Record<string, any>) {
  return requestClient.post<{ executionOrder: string[]; valid: boolean }>(
    '/toonflow/production/validateWorkflow',
    { workflow },
  );
}

export function createWorkflowRun(data: {
  autoStart?: boolean;
  input?: Record<string, any>;
  projectId: number;
  scriptId: number;
  triggerType?: string;
}) {
  return requestClient.post<{
    definitionVersion: number;
    id: number;
    nodeCount: number;
    state: string;
  }>('/toonflow/production/workflowRuns', data);
}

export interface WorkflowRunDetail {
  createTime: number;
  definitionVersion: number;
  errorReason?: string;
  finishTime?: number;
  id: number;
  input: Record<string, any>;
  nodes: WorkflowNodeRun[];
  output?: Record<string, any>;
  startTime?: number;
  state: string;
  triggerType: string;
}

export function getWorkflowRun(id: number) {
  return requestClient.post<WorkflowRunDetail>(
    '/toonflow/production/workflowRuns/state',
    { id },
  );
}

export function cancelWorkflowRun(id: number) {
  return requestClient.post<{ id: number; state: string }>(
    '/toonflow/production/workflowRuns/cancel',
    { id },
  );
}

export interface WorkflowNodeRun {
  attempt: number;
  createTime: number;
  errorReason?: string;
  finishTime?: number;
  id: number;
  input: Record<string, any>;
  nodeId: string;
  nodeType: string;
  output?: Record<string, any>;
  progressCurrent: number;
  progressTotal: number;
  retryOfId?: number;
  startTime?: number;
  state: string;
  workflowRunId: number;
  agentRunId?: number;
}

export function startWorkflowNode(data: {
  input: Record<string, any>;
  nodeId: string;
  workflowRunId: number;
  agentRunId?: number;
}) {
  return requestClient.post<{
    id: number;
    nodeId: string;
    progressCurrent: number;
    progressTotal: number;
    state: string;
    workflowRunId: number;
  }>('/toonflow/production/workflowNodeRuns/start', data);
}

export function getWorkflowNodeRun(id: number) {
  return requestClient.post<WorkflowNodeRun>(
    '/toonflow/production/workflowNodeRuns/state',
    { id },
  );
}

export function getLatestWorkflowNodeRun(
  projectId: number,
  scriptId: number,
  nodeId: string,
) {
  return requestClient.get<null | WorkflowNodeRun>(
    '/toonflow/production/workflowNodeRuns/latest',
    { params: { nodeId, projectId, scriptId } },
  );
}

export function cancelWorkflowNodeRun(id: number) {
  return requestClient.post<{ id: number; state: string }>(
    '/toonflow/production/workflowNodeRuns/cancel',
    { id },
  );
}

export function retryWorkflowNodeRun(id: number) {
  return requestClient.post<{
    id: number;
    nodeId: string;
    progressCurrent: number;
    progressTotal: number;
    state: string;
    workflowRunId: number;
  }>('/toonflow/production/workflowNodeRuns/retry', { id });
}

export function getWorkflowRuns(projectId: number, scriptId: number) {
  return requestClient.get<
    Array<{
      createTime: number;
      definitionVersion: number;
      errorReason?: string;
      finishTime?: number;
      id: number;
      startTime?: number;
      state: string;
      triggerType: string;
    }>
  >('/toonflow/production/workflowRuns', {
    params: { projectId, scriptId },
  });
}

export function getStoryboards(projectId: number, scriptId: number) {
  return requestClient.post<ToonflowApi.Storyboard[]>(
    '/toonflow/production/getStoryboardData',
    { projectId, scriptId },
  );
}

export function addStoryboard(data: Partial<ToonflowApi.Storyboard>) {
  return requestClient.post<{ id: number }>('/toonflow/production/storyboard/addStoryboard', data);
}

export function removeStoryboard(id: number) {
  return requestClient.post('/toonflow/production/storyboard/removeFrame', { id });
}

export function editStoryboardInfo(data: Pick<ToonflowApi.Storyboard, 'associateAssetsIds' | 'cameraMove' | 'duration' | 'id' | 'prompt' | 'sceneKey' | 'sceneStateDescription' | 'sceneStateId' | 'sceneStateKey' | 'sceneStateParentKey' | 'shouldGenerateImage' | 'shotSize' | 'timeOfDay' | 'track' | 'videoDesc'>) {
  return requestClient.post('/toonflow/production/storyboard/editStoryboardInfo', data);
}

export function getSceneConsistencyCatalog(projectId: number, scriptId: number) {
  return requestClient.post<ToonflowApi.SceneConsistencyCatalog>(
    '/toonflow/production/sceneConsistency/catalog',
    { projectId, scriptId },
  );
}

export function autoConfigureSceneConsistency(
  projectId: number,
  scriptId: number,
) {
  return requestClient.post<ToonflowApi.AutoSceneConsistencyResult>(
    '/toonflow/production/sceneConsistency/autoConfigure',
    { projectId, scriptId },
    { timeout: 300_000 },
  );
}

export function saveSceneMaster(data: {
  layoutSpec?: Record<string, unknown>;
  name: string;
  pinnedImageId?: number;
  projectId: number;
  sceneAssetId?: number;
  sceneKey: string;
  scriptId: number;
  spatialPrompt: string;
}) {
  return requestClient.post<{ id: number }>(
    '/toonflow/production/sceneConsistency/saveMaster',
    data,
  );
}

export function saveSceneState(data: {
  changeSummary: string;
  id?: number;
  name: string;
  objectStates?: Record<string, string>;
  parentStateId?: number;
  referenceAssetIds: number[];
  sceneMasterId: number;
  sequence?: number;
  stateKey: string;
  statePrompt: string;
}) {
  return requestClient.post<{ id: number }>(
    '/toonflow/production/sceneConsistency/saveState',
    data,
  );
}

export function reorderStoryboards(projectId: number, scriptId: number, storyboardIds: number[]) {
  return requestClient.post('/toonflow/production/storyboard/reorder', { projectId, scriptId, storyboardIds });
}


export function getAgentDeployments() {
  return requestClient.get<ToonflowApi.AgentDeployment[]>('/toonflow/setting/agentDeploy');
}

export function updateAgentDeployment(data: Partial<ToonflowApi.AgentDeployment> & { id: number }) {
  return requestClient.post('/toonflow/setting/agentDeploy', data);
}

export const getVideoWorkbench = (projectId: number, scriptId: number) => requestClient.post<Record<string, any>>('/production/workbench/getGenerateData', { projectId, scriptId });
export const addVideoTrack = (projectId: number, scriptId: number, duration = 5) => requestClient.post<number>('/production/workbench/addTrack', { projectId, scriptId, duration });
export const updateVideoTrackPrompt = (id: number, prompt: string) => requestClient.post('/production/workbench/updateVideoPrompt', { id, prompt });
export const updateVideoContinuityMode = (id: number, continuityMode: string) => requestClient.post('/production/workbench/updateVideoContinuityMode', { id, continuityMode });
export const updateVideoTransitionSettings = (data: ToonflowApi.UpdateVideoTransitionSettings) => requestClient.post('/production/workbench/updateVideoTransitionSettings', data);
export const generateTrackVideo = (data: Record<string, any>) => requestClient.post<number>('/production/workbench/generateVideo', data);
export const generateVideoPrompt = (data: Record<string, any>) => requestClient.post<string>('/production/workbench/generateVideoPrompt', data);
export const batchGenerateVideoPrompts = (data: Record<string, any>) => requestClient.post('/production/workbench/batchGeneratePrompt', data);
export const batchGenerateVideos = (data: Record<string, any>) => requestClient.post('/production/workbench/batchGenerateVideo', data);
export const reorderVideoTracks = (projectId: number, scriptId: number, trackIds: number[]) => requestClient.post('/production/workbench/reorderTracks', { projectId, scriptId, trackIds });
export const bindTrackStoryboards = (trackId: number, storyboardIds: number[]) => requestClient.post('/production/workbench/bindStoryboards', { trackId, storyboardIds });
export const cancelTrackVideo = (id: number) => requestClient.post('/production/workbench/cancelVideo', { id });
export const inspectTrackVideo = (id: number) => requestClient.post<{ taskId: number }>('/production/workbench/inspectVideo', { id });
export const retryTrackVideo = (data: Record<string, any>) => requestClient.post<number>('/production/workbench/retryVideo', data);
export const pollTrackVideos = (projectId: number, scriptId: number, videoIds: number[]) => requestClient.post<Array<{ id: number; state: string; errorReason?: string; filePath?: string; src?: string; retryOfId?: number; generationContext?: Record<string, unknown> }>>('/production/workbench/checkVideoStateList', { projectId, scriptId, videoIds });
export const exportFinalVideo = (projectId: number, scriptId: number, videoIds: number[] = []) => requestClient.post<{ taskId: number; state: string }>('/production/workbench/exportVideo', { projectId, scriptId, videoIds });
export const selectTrackVideo = (trackId: number, videoId: number) => requestClient.post('/production/workbench/selectVideo', { trackId, videoId });
export const deleteTrackVideo = (id: number) => requestClient.post('/production/workbench/delVideo', { id });
export const getAudioBindAssetsList = (assetsIds: number[]) => requestClient.post<any[]>('/production/workbench/getAudioBindAssetsList', { assetsIds });
export const getWorkbenchFileUrls = (items: Array<{ id: number; sources: 'assets' | 'storyboard' }>) => requestClient.post<{ data: Record<string, string> }>('/production/workbench/getFileUrl', { items });
export const getAssetsWithAudio = (projectId: number) => requestClient.post<Array<ToonflowApi.Asset & { audioUrl?: string; relepedAudio: Array<{ id: number; name: string; url?: string }> }>>('/cornerScape/getAllAssets', { projectId });
export const updateAssetAudio = (assetsId: number, audioIds: number[]) => requestClient.post('/cornerScape/updateAssetsAudio', { assetsId, audioIds });
export const batchBindAudio = (projectId: number, assetsIds: number[]) => requestClient.post('/cornerScape/batchBindAudio', { projectId, assetsIds });
export const pollAssetAudio = (ids: number[]) => requestClient.post<Array<{ id: number; audioBindState?: number }>>('/cornerScape/pollingAudio', { ids });
export const generateAssetDubbing = (data: { projectId: number; assetsId: number; text: string; voice?: string; model?: string }) => requestClient.post<{ id: number; url: string }>('/cornerScape/generateDubbing', data);
export const runAgent = (data: { agentType: 'productionAgent' | 'scriptAgent'; isolationKey: string; projectId: number; scriptId?: number; content: string; think?: boolean; thinkLevel?: number }) => requestClient.post<{ id: number; state: string; content: string }>('/agents/chat', data);
export const startAgent = (data: { agentType: 'productionAgent' | 'scriptAgent'; isolationKey: string; projectId: number; scriptId?: number; content: string; think?: boolean; thinkLevel?: number }) => requestClient.post<{ id: number; state: string }>('/agents/start', data);
export const getAgentRunState = (id: number) => requestClient.post<ToonflowApi.AgentRun>('/agents/runState', { id });
export const stopAgent = (id: number) => requestClient.post<{ id: number; state: string }>('/agents/stop', { id });
export const getAgentRunEvents = (runId: number, afterId = 0) => requestClient.post<ToonflowApi.AgentRunEvent[]>('/agents/events', { runId, afterId });
export const retryAgent = (id: number) => requestClient.post<{ id: number; state: string; retryOf: number }>('/agents/retry', { id });
export const getAgentMemories = (agentType: 'productionAgent' | 'scriptAgent', isolationKey: string) => requestClient.post<ToonflowApi.AgentMemory[]>('/agents/memories', { agentType, isolationKey });
export const getAgentRuns = (agentType: 'productionAgent' | 'scriptAgent', isolationKey: string) => requestClient.post<ToonflowApi.AgentRun[]>('/agents/runs', { agentType, isolationKey });
export const clearAgentMemory = (agentType: 'productionAgent' | 'scriptAgent', isolationKey: string, memoryType: 'all' | 'message' | 'summary' = 'all') => requestClient.post('/agents/clearMemory', { agentType, isolationKey, memoryType });
export const clearAllAgentMemory = (agentType: 'productionAgent' | 'scriptAgent') => requestClient.post<{ deleted: number }>('/agents/deleteAllMemory', { agentType });
export const getScriptAgentPlan = (projectId: number) => requestClient.post<{ id: number; data: { storySkeleton: string; adaptationStrategy: string; script: Array<{ id: number; name: string; content: string }> } }>('/scriptAgent/getPlanData', { projectId, agentType: 'scriptAgent' });
export const saveScriptAgentPlan = (projectId: number, data: Record<string, any>) => requestClient.post<{ id: number }>('/scriptAgent/setPlanData', { projectId, agentType: 'scriptAgent', data });
export const executeAgentTool = (data: { agentType: 'productionAgent' | 'scriptAgent'; projectId: number; scriptId?: number; toolName: string; arguments?: Record<string, any> }) => requestClient.post<{ callId: number; result: any }>('/agents/tools/execute', data);
