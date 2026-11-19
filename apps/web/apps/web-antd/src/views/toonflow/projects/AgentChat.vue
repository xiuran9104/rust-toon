<script setup lang="ts">
/* eslint-disable vue/custom-event-name-casing, vue/no-mutating-props, vue/no-v-html -- the shared session array and legacy event names are the existing AgentChat contract */
import type {
  AgentChatContentBlock,
  AgentChatMessage,
  AgentHistoryFrame,
} from './agent-chat-history';

import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';

import { useAppConfig } from '@vben/hooks';
import { useAccessStore } from '@vben/stores';

import {
  Button,
  Dropdown,
  Menu,
  Select,
  SelectOption,
  Switch,
  Textarea,
} from 'ant-design-vue';

import { buildWebSocketUrl } from '#/utils/websocket';

import { getAgentRunState, stopAgent } from '#/api/toonflow';

import { assetFileUrl } from '../assets/asset-types';
import { useAgentChatRun } from './agent-chat-run';
import {
  AGENT_HISTORY_PAGE_SIZE,
  historyContentPreview,
  mergeAgentHistory,
} from './agent-chat-history';
import { createRunWatcher } from './agent-run-watch';

const props = defineProps<{
  agentType: 'productionAgent' | 'scriptAgent';
  messages: AgentChatMessage[];
  projectId: number;
  scriptId?: number;
  starterLabel?: string;
  starterPrompt?: string;
}>();

const emit = defineEmits<{
  (e: 'activity', payload: { status: string; toolName: string }): void;
  (e: 'clear-memory', memoryType: 'all' | 'message' | 'summary'): void;
  (e: 'run-settled', payload: { runId: number; state: string }): void;
  (e: 'workspace-preview', payload: { key: 'scriptPlan' | 'storyboardTable'; value: string }): void;
  (e: 'tool-result', payload: { result: any; status: string; toolName: string }): void;
}>();

const accessStore = useAccessStore();
const { apiURL } = useAppConfig(import.meta.env, import.meta.env.PROD);
const connected = ref(false);
const run = useAgentChatRun();
const hasActiveRun = run.active;
const connecting = ref(false);
const restoredNotice = ref(false);
const thinkEnabled = ref(true);
const thinkLevel = ref(1);
const ratings = ref<Record<string, 'bad' | 'good' | undefined>>({});
const historyHasMore = ref(false);
const historyLoading = ref(false);
const historyOldestId = ref<number>();
const expandedHistoryBlocks = ref(new Set<string>());

let ws: null | WebSocket = null;
let hasConnected = false;
let restoreTimer: number | undefined;
let reconnectTimer: number | undefined;
let scrollFrame: number | undefined;
let historyFallbackTimer: number | undefined;
let workspacePreviewTimer: number | undefined;
let pendingWorkspacePreviewText = '';
let liveMessageBudget = 0;
let disposed = true;
let shouldReconnect = false;
const completedToolCalls = new Set<string>();
const liveMessageIds = new Set<string>();
const workspacePreviewValues = new Map<string, string>();
const messageById = new Map<string, AgentChatMessage>();
const blockById = new Map<string, AgentChatContentBlock>();

// durable 运行跟踪：run:started 记录本会话发起的运行；run:active 在重连时
// 恢复“执行中”指示并轮询跟随到终态（运行已离开本 socket 的消息流）。
const trackedRunId = ref<number>();
function requestHistoryRefresh() {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(
    JSON.stringify({
      type: 'history',
      beforeId: null,
      limit: AGENT_HISTORY_PAGE_SIZE,
    }),
  );
}
function handleReattachedTerminal(runId: number, state: string) {
  trackedRunId.value = undefined;
  run.reset();
  emit('activity', {
    status: state === 'success' ? 'complete' : state === 'canceled' ? 'canceled' : 'error',
    toolName: '__agent__',
  });
  emit('run-settled', { runId, state });
  requestHistoryRefresh();
}
const runWatcher = createRunWatcher({
  fetchState: (runId) => getAgentRunState(runId),
  onTerminal: handleReattachedTerminal,
});
watch(hasActiveRun, (active) => {
  if (!active) trackedRunId.value = undefined;
});
const workspacePreviewPatterns = {
  scriptPlan: /<scriptPlan>([\s\S]*?)<\/scriptPlan>/,
  storyboardTable: /<storyboardTable>([\s\S]*?)<\/storyboardTable>/,
};

function emitWorkspacePreview(text: string) {
  for (const key of ['scriptPlan', 'storyboardTable'] as const) {
    const match = text.match(workspacePreviewPatterns[key]);
    const value = match?.[1]?.trim();
    if (value) updateWorkspacePreview(key, value);
  }
}

function updateWorkspacePreview(
  key: 'scriptPlan' | 'storyboardTable',
  value: string,
) {
  if (workspacePreviewValues.get(key) === value) return;
  workspacePreviewValues.set(key, value);
  emit('workspace-preview', { key, value });
}

function restoreWorkspacePreviews(messages: AgentChatMessage[]) {
  const pending = new Set(['scriptPlan', 'storyboardTable'] as const);
  for (const message of [...messages].reverse()) {
    for (const block of [...message.content].reverse()) {
      if (typeof block.data !== 'string') continue;
      for (const key of [...pending]) {
        const value = block.data.match(workspacePreviewPatterns[key])?.[1]?.trim();
        if (!value) continue;
        updateWorkspacePreview(key, value);
        pending.delete(key);
      }
      if (pending.size === 0) return;
    }
  }
}

function queueWorkspacePreview(text: string, complete = false) {
  pendingWorkspacePreviewText = text;
  if (complete) {
    window.clearTimeout(workspacePreviewTimer);
    workspacePreviewTimer = undefined;
    emitWorkspacePreview(pendingWorkspacePreviewText);
    return;
  }
  if (workspacePreviewTimer) return;
  workspacePreviewTimer = window.setTimeout(() => {
    workspacePreviewTimer = undefined;
    emitWorkspacePreview(pendingWorkspacePreviewText);
  }, 200);
}

const isolationKey = computed(() => {
  const sid = props.agentType === 'productionAgent' ? (props.scriptId ?? 'none') : 'project';
  return `${props.agentType}:${props.projectId}:${sid}`;
});

function rebuildMessageIndex() {
  messageById.clear();
  blockById.clear();
  for (const message of props.messages) {
    messageById.set(message.id, message);
    for (const block of message.content) blockById.set(block.id, block);
  }
}

function replaceMessages(messages: AgentChatMessage[]) {
  props.messages.splice(0, props.messages.length, ...messages);
  rebuildMessageIndex();
}

function resetHistoryState() {
  historyHasMore.value = false;
  historyLoading.value = true;
  historyOldestId.value = undefined;
  expandedHistoryBlocks.value = new Set();
}

function clearReconnectTimer() {
  window.clearTimeout(reconnectTimer);
  reconnectTimer = undefined;
}

function scheduleReconnect() {
  if (disposed || !shouldReconnect) return;
  clearReconnectTimer();
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = undefined;
    if (!disposed && shouldReconnect && !connected.value && !connecting.value) {
      connect();
    }
  }, 2000);
}

function connect() {
  shouldReconnect = true;
  if (disposed) return;
  if (ws && ws.readyState === WebSocket.OPEN) return;
  if (ws && ws.readyState === WebSocket.CONNECTING) return;
  const token = accessStore.accessToken;
  if (!token) {
    console.warn('[AgentChat] No access token, skipping WebSocket connect');
    connecting.value = false;
    return;
  }
  clearReconnectTimer();
  connecting.value = true;
  const params = new URLSearchParams({
    historyMode: 'batch',
    token: token ?? '',
    isolationKey: isolationKey.value,
    projectId: String(props.projectId),
  });
  if (props.scriptId) params.set('scriptId', String(props.scriptId));
  const url = buildWebSocketUrl(
    apiURL,
    `/socket/${props.agentType}`,
    params,
    location.href,
  );

  const socket = new WebSocket(url);
  ws = socket;

  socket.onopen = () => {
    if (ws !== socket || disposed) {
      socket.close();
      return;
    }
    replaceMessages([]);
    resetHistoryState();
    completedToolCalls.clear();
    liveMessageIds.clear();
    liveMessageBudget = 0;
    window.clearTimeout(historyFallbackTimer);
    historyFallbackTimer = window.setTimeout(() => {
      historyLoading.value = false;
    }, 750);
    connected.value = true;
    connecting.value = false;
    updateThinkConfig(thinkEnabled.value, thinkLevel.value);
    if (hasConnected) {
      restoredNotice.value = true;
      window.clearTimeout(restoreTimer);
      restoreTimer = window.setTimeout(() => { restoredNotice.value = false; }, 3000);
    }
    hasConnected = true;
  };

  socket.onmessage = (event) => {
    if (ws !== socket || disposed) return;
    try {
      const frame = JSON.parse(event.data);
      handleServerMessage(frame);
    } catch { /* ignore invalid frames */ }
  };

  socket.onerror = () => {
    if (ws !== socket) return;
    run.reset();
    connecting.value = false;
    connected.value = false;
    scheduleReconnect();
  };

  socket.onclose = () => {
    if (ws !== socket) return;
    run.reset();
    ws = null;
    connected.value = false;
    connecting.value = false;
    scheduleReconnect();
  };
}

function disconnect() {
  run.reset();
  shouldReconnect = false;
  clearReconnectTimer();
  const socket = ws;
  ws = null;
  if (socket) {
    socket.onopen = null;
    socket.onmessage = null;
    socket.onerror = null;
    socket.onclose = null;
    socket.close();
  }
  connected.value = false;
  connecting.value = false;
}

function send(content: string) {
  if (!ws || ws.readyState !== WebSocket.OPEN || hasActiveRun.value || !content.trim()) return;
  liveMessageBudget = 2;
  ws.send(JSON.stringify({ type: 'chat', content }));
  run.begin();
}

function stop() {
  // 本会话消息流走 WS stop；被跟踪的 durable 运行（含重连挂载的）同时
  // 调 HTTP stop，服务端置 cancel_requested 并 abort 本进程任务。
  if (trackedRunId.value) {
    const runId = trackedRunId.value;
    trackedRunId.value = undefined;
    runWatcher.stop();
    void stopAgent(runId).catch(() => {
      // 运行可能已自然结束；守卫式停止失败无需提示。
    });
  }
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(JSON.stringify({ type: 'stop' }));
  run.reset();
  liveMessageBudget = 0;
  liveMessageIds.clear();
}

function updateThinkConfig(think: boolean, thinkLevel: number) {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(JSON.stringify({ type: 'updateThinkConfig', think, thinkLevel }));
}
function applyThinkConfig() {
  updateThinkConfig(thinkEnabled.value, thinkLevel.value);
}

function applyHistoryFrame(data: AgentHistoryFrame) {
  const incoming = Array.isArray(data.messages) ? data.messages : [];
  const container = chatContainer.value;
  const previousHeight = container?.scrollHeight ?? 0;
  const previousTop = container?.scrollTop ?? 0;
  replaceMessages(mergeAgentHistory(props.messages, incoming, !!data.prepend));
  restoreWorkspacePreviews(incoming);
  historyHasMore.value = !!data.hasMore;
  historyOldestId.value = data.oldestId;
  historyLoading.value = false;
  window.clearTimeout(historyFallbackTimer);
  historyFallbackTimer = undefined;

  if (data.prepend && container) {
    void nextTick(() => {
      window.requestAnimationFrame(() => {
        container.scrollTop = previousTop + container.scrollHeight - previousHeight;
      });
    });
  } else {
    scrollToBottom(true);
  }
}

function loadOlderHistory() {
  if (
    historyLoading.value ||
    !historyHasMore.value ||
    !historyOldestId.value ||
    !ws ||
    ws.readyState !== WebSocket.OPEN
  ) {
    return;
  }
  historyLoading.value = true;
  ws.send(JSON.stringify({
    type: 'history',
    beforeId: historyOldestId.value,
    limit: AGENT_HISTORY_PAGE_SIZE,
  }));
}

function historyBlockKey(message: AgentChatMessage, block: AgentChatContentBlock) {
  return `${message.id}:${block.id}`;
}

function isHistoryBlockExpanded(
  message: AgentChatMessage,
  block: AgentChatContentBlock,
) {
  return expandedHistoryBlocks.value.has(historyBlockKey(message, block));
}

function historyBlockPreview(
  message: AgentChatMessage,
  block: AgentChatContentBlock,
) {
  return historyContentPreview(
    block.data,
    isHistoryBlockExpanded(message, block),
  );
}

function toggleHistoryBlock(
  message: AgentChatMessage,
  block: AgentChatContentBlock,
) {
  const key = historyBlockKey(message, block);
  const next = new Set(expandedHistoryBlocks.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expandedHistoryBlocks.value = next;
}

function handleServerMessage(frame: any) {
  const { event: type, data } = frame;
  if (!data) return;

  switch (type) {
    case 'content:add': {
      const msg = messageById.get(data.messageId);
      if (msg) {
        msg.content.push(data.content);
        blockById.set(data.content.id, msg.content[msg.content.length - 1]!);
        if (data.content?.type === 'toolcall') {
          emit('activity', {
            status: data.content.status || 'pending',
            toolName: data.content.data?.toolCallName || 'unknown',
          });
        }
        scrollToBottom();
      }
      break;
    }
    case 'content:update': {
      const block = blockById.get(data.contentId);
      if (block) {
        block.status = data.status || block.status;
        if (data.strategy === 'append' && data.data !== undefined) {
          if (block.type === 'text' || block.type === 'markdown') {
            block.data = (block.data || '') + (typeof data.data === 'string' ? data.data : '');
            queueWorkspacePreview(
              String(block.data || ''),
              data.status === 'complete',
            );
          } else if (block.type === 'thinking') {
            if (typeof data.data === 'object' && data.data.text) {
              block.data.text = (block.data.text || '') + data.data.text;
            } else if (block.data.title && data.data?.title) {
              block.data.title = data.data.title;
            }
          } else if (block.type === 'toolcall') {
            if (data.data.toolCallId) block.data.toolCallId = data.data.toolCallId;
            if (data.data.toolCallName) block.data.toolCallName = data.data.toolCallName;
            if (data.data.args) block.data.args = (block.data.args || '') + data.data.args;
            if (data.data.chunk) block.data.result = (block.data.result || '') + data.data.chunk;
          }
        } else if (data.strategy === 'merge' && data.data !== undefined) {
          if (typeof data.data === 'object') {
            Object.assign(block.data, data.data);
          } else {
            block.data = data.data;
          }
        }
        if (
          block.type === 'toolcall' &&
          (data.status === 'complete' || data.status === 'error') &&
          !completedToolCalls.has(block.id)
        ) {
          completedToolCalls.add(block.id);
          const toolName = block.data.toolCallName || 'unknown';
          emit('activity', { status: data.status, toolName });
          emit('tool-result', {
            toolName,
            status: data.status,
            result: data.data?.result || block.data.result || block.data,
          });
        }
        scrollToBottom();
      }
      break;
    }
    case 'history': {
      applyHistoryFrame(data as AgentHistoryFrame);
      break;
    }
    case 'history:error': {
      historyLoading.value = false;
      break;
    }
    case 'run:started': {
      // 本会话发起的 durable 运行：记录 ID 供停止按钮跨会话调用。
      if (data.runId) trackedRunId.value = data.runId;
      break;
    }
    case 'run:active': {
      // 重连时后端仍有运行中的任务：恢复“执行中”指示并轮询跟随到终态。
      const runId = data.runId;
      if (runId && !hasActiveRun.value) {
        trackedRunId.value = runId;
        run.begin();
        runWatcher.watch(runId);
      }
      break;
    }
    case 'message': {
      const msg: AgentChatMessage = {
        id: data.id,
        role: data.role || 'assistant',
        name: data.name,
        status: data.status || 'pending',
        datetime: data.datetime || new Date().toISOString(),
        content: [],
      };
      props.messages.push(msg);
      messageById.set(msg.id, props.messages[props.messages.length - 1]!);
      run.message(msg.id, msg.role);
      if (liveMessageBudget > 0) {
        liveMessageIds.add(msg.id);
        liveMessageBudget -= 1;
      }
      scrollToBottom();
      break;
    }
    case 'message:update': {
      run.update(data.id, data.status);
      const msg = messageById.get(data.id);
      if (msg) {
        msg.status = data.status;
        if (data.ext) Object.assign(msg, { ext: data.ext });
      }
      if (liveMessageIds.has(data.id)) {
        emit('activity', { status: data.status || 'pending', toolName: '__agent__' });
        if (data.status === 'complete' || data.status === 'error') {
          liveMessageIds.delete(data.id);
        }
      }
      break;
    }
  }
}

const chatContainer = ref<HTMLElement | null>(null);

// 跟随滚动的真相源是用户的滚动位置：贴近底部（≤40px）视为“跟随”，
// 一旦向上滚离就停止自动跟随，直到用户滚回底部或点击“回到底部”。
// 内容增长本身不触发 scroll 事件，因此流式追加不会误关跟随。
const stickToBottom = ref(true);
const hasUnreadBelow = ref(false);

function handleChatScroll() {
  const container = chatContainer.value;
  if (!container) return;
  const distanceFromBottom =
    container.scrollHeight - container.scrollTop - container.clientHeight;
  stickToBottom.value = distanceFromBottom <= 40;
  if (stickToBottom.value) hasUnreadBelow.value = false;
}

function scrollToBottom(force = false) {
  if (!force && !stickToBottom.value) {
    // 用户正在上方阅读：不打扰，只提示有新内容。
    hasUnreadBelow.value = true;
    return;
  }
  if (scrollFrame !== undefined) return;
  scrollFrame = window.requestAnimationFrame(() => {
    scrollFrame = undefined;
    if (chatContainer.value) {
      chatContainer.value.scrollTop = chatContainer.value.scrollHeight;
    }
  });
}

function jumpToBottom() {
  stickToBottom.value = true;
  hasUnreadBelow.value = false;
  scrollToBottom(true);
}

onBeforeUnmount(() => {
  disposed = true;
  runWatcher.dispose();
  window.clearTimeout(restoreTimer);
  window.clearTimeout(historyFallbackTimer);
  window.clearTimeout(workspacePreviewTimer);
  if (scrollFrame !== undefined) window.cancelAnimationFrame(scrollFrame);
  chatContainer.value?.removeEventListener('scroll', handleChatScroll);
  disconnect();
});

function toolCallStatusIcon(status: string) {
  if (status === 'streaming') return '⟳';
  if (status === 'complete') return '✓';
  if (status === 'error') return '✗';
  return '…';
}

function toolCallStatusColor(status: string) {
  if (status === 'complete') return 'var(--ant-color-success)';
  if (status === 'error') return 'var(--ant-color-error)';
  return 'var(--ant-color-primary)';
}

const inputText = ref('');

function handleKeydown(e: KeyboardEvent) {
  if (e.key !== 'Enter') return;
  if (e.ctrlKey || e.metaKey || e.shiftKey) {
    // Ctrl+Enter / Shift+Enter = newline
    return;
  }
  // Plain Enter = send
  e.preventDefault();
  e.stopPropagation();
  handleSend();
}

function handleSend() {
  if (hasActiveRun.value) return;
  const text = inputText.value.trim();
  if (!text) return;
  if (!connected.value) {
    connect();
    return;
  }
  inputText.value = '';
  send(text);
}

function handleStarter() {
  if (!props.starterPrompt) return;
  inputText.value = props.starterPrompt;
  handleSend();
}

function blockItems(data: any): any[] {
  if (Array.isArray(data)) return data;
  if (Array.isArray(data?.items)) return data.items;
  if (Array.isArray(data?.results)) return data.results;
  return data ? [data] : [];
}

function blockUrl(item: any) {
  return assetFileUrl(
    typeof item === 'string' ? item : item?.url || item?.src || item?.fileUrl || '',
  );
}

function suggestionText(item: any) {
  return typeof item === 'string' ? item : item?.text || item?.label || item?.content || '';
}

function useSuggestion(item: any) {
  inputText.value = suggestionText(item);
}

// Auto-connect on mount and when agentType/projectId/scriptId changes
onMounted(() => {
  disposed = false;
  rebuildMessageIndex();
  chatContainer.value?.addEventListener('scroll', handleChatScroll, {
    passive: true,
  });
  connect();
});

watch(
  isolationKey,
  () => {
    disconnect();
    replaceMessages([]);
    resetHistoryState();
    completedToolCalls.clear();
    liveMessageIds.clear();
    workspacePreviewValues.clear();
    hasConnected = false;
    connect();
  },
);

watch(() => props.messages, rebuildMessageIndex, { immediate: true });

defineExpose({ connect, disconnect, send, stop, updateThinkConfig, connected });
</script>

<template>
  <div class="agent-chat-wrapper">
    <!-- Connection status -->
    <div class="connection-status" :class="{ connected, connecting }">
      <span class="status-dot"></span>
      <span class="status-text">
        {{ connecting ? '连接中...' : connected ? (hasActiveRun ? 'Agent 执行中' : '已连接 · 等待发送') : '未连接' }}
      </span>
      <div class="agent-controls">
        <label><Switch v-model:checked="thinkEnabled" size="small" @change="applyThinkConfig" /> 深度思考</label>
        <Select v-model:value="thinkLevel" size="small" :disabled="!thinkEnabled" style="width: 88px" @change="applyThinkConfig">
          <SelectOption :value="0">快速</SelectOption><SelectOption :value="1">基础</SelectOption><SelectOption :value="2">深入</SelectOption><SelectOption :value="3">完整</SelectOption>
        </Select>
        <Dropdown>
          <Button size="small">记忆</Button>
          <template #overlay>
            <Menu>
              <Menu.Item @click="emit('clear-memory', 'message')">清除消息记忆</Menu.Item>
              <Menu.Item @click="emit('clear-memory', 'summary')">清除摘要记忆</Menu.Item>
              <Menu.Item danger @click="emit('clear-memory', 'all')">清除全部记忆</Menu.Item>
            </Menu>
          </template>
        </Dropdown>
      </div>
    </div>
    <div v-if="restoredNotice" class="restored-notice">✓ 已恢复会话</div>

    <!-- Messages area -->
    <div ref="chatContainer" class="chat-messages">
      <div v-if="historyHasMore" class="history-more">
        <Button size="small" :loading="historyLoading" @click="loadOlderHistory">
          加载更早会话
        </Button>
      </div>
      <div v-if="messages.length === 0" class="chat-empty">
        <div>让 Agent 读取当前剧本和资产并启动制作流程</div>
        <Button
          v-if="starterPrompt"
          class="starter-button"
          type="primary"
          :disabled="!connected"
          @click="handleStarter"
        >
          {{ starterLabel || '开始制作' }}
        </Button>
      </div>

      <div
        v-for="msg in messages"
        :key="msg.id"
        class="chat-message"
        :class="[`chat-message-${msg.role.replace('assistant', 'agent')}`]"
      >
        <!-- Message header -->
        <div class="message-header">
          <span class="message-role">
            {{ msg.role.startsWith('user') ? '你' : (msg.name || 'Agent') }}
          </span>
          <span class="message-time">{{ new Date(msg.datetime).toLocaleTimeString() }}</span>
        </div>

        <!-- Content blocks -->
        <div v-for="block in msg.content" :key="block.id" class="content-block">
          <!-- Text content -->
          <div v-if="block.type === 'text' || block.type === 'markdown'" class="content-text">
            <template v-if="msg.historical">
              <div class="history-content-text">{{ historyBlockPreview(msg, block).text }}</div>
              <button
                v-if="historyBlockPreview(msg, block).truncated"
                class="history-expand"
                type="button"
                @click="toggleHistoryBlock(msg, block)"
              >
                {{ isHistoryBlockExpanded(msg, block) ? '收起内容' : '展开完整内容' }}
              </button>
            </template>
            <template v-else>
              <div v-if="block.status === 'streaming'" class="streaming-text">{{ block.data }}</div>
              <div v-else class="markdown-body" v-html="block.data"></div>
              <span v-if="block.status === 'streaming'" class="streaming-cursor">▊</span>
            </template>
          </div>

          <!-- Thinking content -->
          <details
            v-else-if="block.type === 'thinking'"
            :id="block.id"
            class="content-thinking"
            :open="block.status !== 'complete'"
          >
            <summary class="thinking-title">
              {{ block.data?.title || '思考中...' }}
            </summary>
            <div class="thinking-body">{{ block.data?.text }}</div>
          </details>

          <!-- Tool call content -->
          <details
            v-else-if="block.type === 'toolcall'"
            class="content-toolcall"
            :open="block.status === 'streaming'"
          >
            <summary class="toolcall-title">
              <span
                class="toolcall-status-icon"
                :style="{ color: toolCallStatusColor(block.status) }"
              >
                {{ toolCallStatusIcon(block.status) }}
              </span>
              工具调用：{{ block.data?.toolCallName || 'unknown' }}
            </summary>
            <div class="toolcall-body">
              <div v-if="block.data?.args" class="toolcall-section">
                <strong>参数：</strong>
                <pre>{{ block.data.args }}</pre>
              </div>
              <div v-if="block.data?.result" class="toolcall-section">
                <strong>结果：</strong>
                <pre>{{ block.data.result }}</pre>
              </div>
            </div>
          </details>

          <div v-else-if="block.type === 'image'" class="content-images">
            <a v-for="(item, index) in blockItems(block.data)" :key="index" :href="blockUrl(item)" target="_blank" rel="noreferrer">
              <img :src="blockUrl(item)" :alt="item?.name || `生成图片 ${index + 1}`" decoding="async" loading="lazy" />
            </a>
          </div>

          <div v-else-if="block.type === 'attachment'" class="content-attachments">
            <a v-for="(item, index) in blockItems(block.data)" :key="index" :href="blockUrl(item)" target="_blank" rel="noreferrer">
              <span>📎</span><span>{{ item?.name || item?.filename || `附件 ${index + 1}` }}</span><small>{{ item?.size || item?.mimeType || '' }}</small>
            </a>
          </div>

          <div v-else-if="block.type === 'suggestion'" class="content-suggestions">
            <button v-for="(item, index) in blockItems(block.data)" :key="index" type="button" @click="useSuggestion(item)">{{ suggestionText(item) }}</button>
          </div>

          <div v-else-if="block.type === 'search'" class="content-search">
            <b>搜索结果</b>
            <a v-for="(item, index) in blockItems(block.data)" :key="index" :href="blockUrl(item)" target="_blank" rel="noreferrer">{{ item?.title || item?.name || blockUrl(item) }}</a>
          </div>
        </div>

        <!-- Status indicator -->
        <div v-if="msg.status === 'pending' || msg.status === 'streaming'" class="message-loading">
          <span class="loading-dots">...</span>
        </div>
        <div v-else-if="msg.status === 'error'" class="message-error">
          {{ msg.ext?.error || '执行出错' }}
        </div>
        <div v-if="msg.role === 'assistant' && msg.status === 'complete'" class="message-rating">
          <button :class="{ active: ratings[msg.id] === 'good' }" type="button" title="有帮助" @click="ratings[msg.id] = ratings[msg.id] === 'good' ? undefined : 'good'">👍</button>
          <button :class="{ active: ratings[msg.id] === 'bad' }" type="button" title="需要改进" @click="ratings[msg.id] = ratings[msg.id] === 'bad' ? undefined : 'bad'">👎</button>
        </div>
      </div>

      <!-- 用户滚离底部时吸附在可视区底部的回底入口 -->
      <div v-if="!stickToBottom" class="jump-to-bottom">
        <Button size="small" shape="round" @click="jumpToBottom">
          {{ hasUnreadBelow ? '↓ 有新消息' : '↓ 回到底部' }}
        </Button>
      </div>
    </div>

    <!-- Input area -->
    <div class="chat-input-area">
      <div class="input-wrapper">
        <Textarea
          v-model:value="inputText"
          :auto-size="{ minRows: 2, maxRows: 6 }"
          placeholder="输入消息，Enter 发送，Ctrl+Enter 换行"
          @keydown="handleKeydown"
        />
        <Button
          v-if="hasActiveRun"
          class="send-btn stop-btn"
          danger
          :disabled="!connected"
          aria-label="停止生成"
          @click="stop"
        >
          停止
        </Button>
        <Button
          v-else
          class="send-btn"
          type="primary"
          :disabled="!connected || !inputText.trim()"
          aria-label="发送消息"
          @click="handleSend"
        >
          发送
        </Button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.agent-chat-wrapper {
  display: flex;
  flex-direction: column;
  height: 680px;
  border: 1px solid var(--toon-line);
  border-radius: 8px;
  overflow: hidden;
  background: var(--toon-canvas);
}

.connection-status {
  padding: 4px 12px;
  font-size: 12px;
  display: flex;
  align-items: center;
  gap: 6px;
  background: var(--toon-panel);
  border-bottom: 1px solid var(--toon-line);
}
.restored-notice { padding: 7px 12px; color: var(--ant-color-success-text); font-size: 12px; text-align: center; background: var(--ant-color-success-bg); border-bottom: 1px solid var(--ant-color-success-border); }
.connection-status.connected .status-dot { background: var(--ant-color-success); }
.connection-status.connecting .status-dot { background: var(--ant-color-warning); }
.status-dot {
  width: 8px; height: 8px;
  border-radius: 50%;
  background: var(--toon-line);
}
.status-text { color: var(--ant-color-text-tertiary); }
.agent-controls { display: flex; align-items: center; gap: 6px; margin-left: auto; }
.agent-controls label { display: flex; align-items: center; gap: 4px; white-space: nowrap; }

.chat-messages {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.history-more { display: flex; justify-content: center; }

/* 吸附在滚动可视区底部中央的回底按钮：sticky 不依赖外层定位 */
.jump-to-bottom {
  position: sticky;
  bottom: 8px;
  align-self: center;
  z-index: 5;
}

.chat-empty {
  text-align: center;
  color: var(--ant-color-text-tertiary);
  padding: 60px 0;
  font-size: 14px;
}
.starter-button { margin-top: 12px; }

.chat-message {
  max-width: 85%;
  padding: 10px 14px;
  border-radius: 10px;
  font-size: 14px;
  line-height: 1.6;
  content-visibility: auto;
  contain-intrinsic-size: auto 160px;
}

.chat-message-user {
  align-self: flex-end;
  background: var(--ant-color-primary);
  color: #fff;
}
.chat-message-user .message-header { color: rgba(255,255,255,0.7); }
.chat-message-user .message-time { color: rgba(255,255,255,0.5); }

.chat-message-agent {
  align-self: flex-start;
  background: var(--toon-panel);
  border: 1px solid var(--toon-line);
}

.message-header {
  display: flex;
  justify-content: space-between;
  margin-bottom: 6px;
  font-size: 12px;
}
.message-role { font-weight: 600; }
.message-time { color: var(--ant-color-text-tertiary); }

.content-block { margin-top: 4px; }
.content-images { display: grid; gap: 8px; grid-template-columns: repeat(2, minmax(0, 1fr)); }.content-images img { display: block; width: 100%; max-height: 220px; border-radius: 9px; object-fit: cover; }
.content-attachments { display: grid; gap: 6px; }.content-attachments a { display: grid; align-items: center; padding: 9px 10px; border: 1px solid var(--toon-line); border-radius: 9px; color: inherit; background: var(--toon-canvas); grid-template-columns: 24px minmax(0, 1fr) auto; }.content-attachments small { color: var(--ant-color-text-tertiary); }
.content-suggestions { display: flex; flex-wrap: wrap; gap: 7px; }.content-suggestions button { padding: 6px 11px; border: 1px solid var(--toon-line); border-radius: 999px; color: var(--toon-ink); background: var(--toon-panel); cursor: pointer; }.content-suggestions button:hover { border-color: var(--toon-ink); }
.content-search { display: grid; gap: 5px; padding: 10px; border-left: 3px solid var(--toon-ink); background: var(--toon-canvas); }.content-search a { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.message-rating { display: flex; justify-content: flex-end; gap: 3px; margin-top: 5px; }.message-rating button { padding: 2px 5px; border: 0; border-radius: 6px; opacity: .45; background: transparent; cursor: pointer; }.message-rating button:hover, .message-rating button.active { opacity: 1; background: var(--toon-line); }

.content-text {
  word-break: break-word;
}
.history-content-text,
.streaming-text { white-space: pre-wrap; }
.history-expand {
  margin-top: 4px;
  padding: 0;
  border: 0;
  color: var(--ant-color-primary);
  background: transparent;
  cursor: pointer;
  font-size: 12px;
}
.streaming-cursor {
  animation: blink 1s infinite;
  color: var(--ant-color-primary);
}
@keyframes blink { 50% { opacity: 0; } }

.content-thinking {
  background: var(--toon-canvas);
  border-radius: 6px;
  padding: 8px;
  margin: 4px 0;
}
.thinking-title { font-size: 12px; color: var(--ant-color-text-tertiary); cursor: pointer; }
.thinking-body {
  margin-top: 6px;
  font-size: 13px;
  color: var(--toon-muted);
  white-space: pre-wrap;
  max-height: 200px;
  overflow-y: auto;
}

.content-toolcall {
  background: var(--ant-color-warning-bg);
  border: 1px solid var(--ant-color-warning-border);
  border-radius: 6px;
  padding: 8px;
  margin: 4px 0;
}
.toolcall-title {
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 4px;
}
.toolcall-status-icon { font-size: 14px; }
.toolcall-body { margin-top: 6px; }
.toolcall-section {
  margin-bottom: 4px;
  font-size: 12px;
  color: var(--toon-muted);
}
.toolcall-section pre {
  background: var(--toon-panel);
  padding: 6px;
  border-radius: 4px;
  font-size: 11px;
  max-height: 120px;
  overflow: auto;
  margin: 2px 0;
}

.message-loading { color: var(--ant-color-primary); font-size: 12px; }
.message-error { color: var(--ant-color-error); font-size: 12px; }

.chat-input-area {
  padding: 10px 12px;
  background: var(--toon-panel);
  border-top: 1px solid var(--toon-line);
}
.input-wrapper {
  position: relative;
}
.send-btn {
  position: absolute;
  right: 8px;
  bottom: 8px;
  width: 56px;
  height: 32px;
  min-width: 56px;
  padding: 0;
  border-radius: 6px;
  font-size: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1;
}
.stop-btn {
  animation: pulse-stop 1.5s infinite;
}
@keyframes pulse-stop {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.6; }
}
.input-wrapper :deep(.ant-input) {
  padding-right: 72px;
}
</style>
