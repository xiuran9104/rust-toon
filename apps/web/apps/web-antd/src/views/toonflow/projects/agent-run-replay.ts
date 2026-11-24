/**
 * Replays a durable agent run from its persisted events.
 *
 * A reconnect learns from `run:active` that a run is still executing, but the
 * run's output no longer streams over this socket. The events are durable, so
 * they are folded into chat content blocks here: text deltas into text blocks,
 * tool calls into toolcall blocks. Arguments and results are not persisted, so
 * a replayed tool call shows only its name and outcome.
 */

import type { ToonflowApi } from '#/api/toonflow';

import type { AgentChatMessage } from './agent-chat-history';

type RunEvent = ToonflowApi.AgentRunEvent;

/** 与 events 端点每页的 LIMIT 一致：不足一页说明已经追平。 */
const PAGE_SIZE = 500;
/** 首次追平可能跨越多页；每轮同步最多翻的页数，余下的留给下一轮。 */
const MAX_PAGES_PER_SYNC = 20;
/** 连续失败这么多次后放弃回放，运行状态仍由 watcher 跟随。 */
const MAX_CONSECUTIVE_FAILURES = 5;

export function createReplayMessage(
  runId: number,
  startTime = Date.now(),
): AgentChatMessage {
  return {
    content: [],
    datetime: new Date(startTime).toISOString(),
    ext: { replay: true },
    id: `run:${runId}:replay`,
    name: '统筹',
    role: 'assistant',
    status: 'streaming',
  };
}

/**
 * 把一批按 ID 升序到达的事件并入回放消息，返回是否出现终态事件。
 *
 * 文本按工具调用分段：工具调用之后的第一段文字开启新的文本块，回放读起来
 * 才是一份带步骤的记录。文本块始终保持 streaming——消息在运行结束后会被
 * 权威历史替换，不需要在这里把内容交给 HTML 渲染。
 */
export function reduceRunEvents(
  message: AgentChatMessage,
  events: RunEvent[],
): boolean {
  let terminal = false;
  for (const event of events) {
    const data = event.data ?? {};
    switch (event.eventType) {
      case 'delta': {
        if (!data.text) break;
        const last = message.content.at(-1);
        let text =
          last?.type === 'text'
            ? last
            : {
                data: '',
                id: `${message.id}:text:${event.id}`,
                status: 'streaming',
                type: 'text',
              };
        if (text !== last) message.content.push(text);
        text.data += data.text;
        break;
      }
      case 'tool_call': {
        message.content.push({
          data: {
            args: '',
            result: '',
            toolCallId: `replay:${event.id}`,
            toolCallName: data.tool ?? 'unknown',
          },
          id: `${message.id}:tool:${event.id}`,
          status: 'pending',
          type: 'toolcall',
        });
        break;
      }
      case 'tool_result': {
        // 事件里没有配对用的调用 ID，按工具名与最早未完成的调用配对。
        const pending = message.content.find(
          (block) =>
            block.type === 'toolcall' &&
            block.status === 'pending' &&
            block.data?.toolCallName === (data.tool ?? 'unknown'),
        );
        if (!pending) break;
        const failed = data.success === false;
        pending.status = failed ? 'error' : 'complete';
        pending.data.result = failed
          ? `执行失败：${data.error ?? ''}`
          : '';
        break;
      }
      case 'completed':
      case 'failed': {
        terminal = true;
        break;
      }
      default: {
        break;
      }
    }
  }
  return terminal;
}

export interface RunReplayerOptions {
  fetchEvents: (runId: number, afterId: number) => Promise<RunEvent[]>;
  intervalMs?: number;
  /** `changed` 为真表示这次回调确实并入了新事件，调用方据此决定是否打扰用户。 */
  onUpdate: (changed: boolean, message: AgentChatMessage) => void;
}

export interface RunReplayer {
  /** 每次更新后回调；首次回调发生在首轮同步之前，便于调用方先挂载消息。 */
  start: (message: AgentChatMessage, runId: number) => void;
  stop: () => void;
}

/**
 * 增量拉取事件并并入回放消息。传入的消息应由调用方做成响应式，
 * 这样合并过程中的修改才会触发界面更新。
 */
export function createRunReplayer(options: RunReplayerOptions): RunReplayer {
  const intervalMs = options.intervalMs ?? 2000;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let active:
    | undefined
    | { cursor: number; failures: number; message: AgentChatMessage; runId: number };

  function stop() {
    if (timer) clearTimeout(timer);
    timer = undefined;
    active = undefined;
  }

  async function sync() {
    const state = active;
    if (!state) return;
    let merged = 0;
    let terminal = false;
    try {
      for (let page = 0; page < MAX_PAGES_PER_SYNC; page++) {
        if (active !== state || terminal) break;
        const events =
          (await options.fetchEvents(state.runId, state.cursor)) ?? [];
        // 拉取期间被停止或切换到别的运行：这一批作废。
        if (active !== state) return;
        if (events.length === 0) break;
        state.cursor = events.at(-1)!.id;
        merged += events.length;
        terminal = reduceRunEvents(state.message, events);
        // 不足一页即已追平。否则流式输出持续产生事件时，这一轮会一直
        // 翻到恰好拿到空页为止。
        if (events.length < PAGE_SIZE) break;
      }
      state.failures = 0;
    } catch {
      state.failures += 1;
      if (state.failures >= MAX_CONSECUTIVE_FAILURES) {
        stop();
        return;
      }
    }
    if (active !== state) return;
    options.onUpdate(merged > 0, state.message);
    if (terminal) {
      stop();
      return;
    }
    timer = setTimeout(() => void sync(), intervalMs);
  }

  return {
    start(message, runId) {
      stop();
      active = { cursor: 0, failures: 0, message, runId };
      options.onUpdate(false, message);
      void sync();
    },
    stop,
  };
}
