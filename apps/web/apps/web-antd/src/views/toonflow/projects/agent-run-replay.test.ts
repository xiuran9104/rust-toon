import type { ToonflowApi } from '#/api/toonflow';

import { describe, expect, it, vi } from 'vitest';

import { createReplayMessage, createRunReplayer, reduceRunEvents } from './agent-run-replay';

type RunEvent = ToonflowApi.AgentRunEvent;

function event(id: number, eventType: string, data: RunEvent['data'] = {}) {
  return { createTime: id, data, eventType, id, runId: 7 };
}

describe('reduceRunEvents', () => {
  it('folds deltas into text blocks that split at tool calls', () => {
    const message = createReplayMessage(7, 1_000);
    const terminal = reduceRunEvents(message, [
      event(1, 'started', { message: 'Agent 已启动' }),
      event(2, 'delta', { text: '先读取' }),
      event(3, 'delta', { text: '章节。' }),
      event(4, 'tool_call', { tool: 'get_novel_events' }),
      event(5, 'tool_result', { tool: 'get_novel_events', success: true }),
      event(6, 'delta', { text: '接下来生成骨架。' }),
    ]);
    expect(terminal).toBe(false);
    expect(message.content.map((block) => block.type)).toEqual([
      'text',
      'toolcall',
      'text',
    ]);
    expect(message.content[0]!.data).toBe('先读取章节。');
    expect(message.content[2]!.data).toBe('接下来生成骨架。');
    expect(message.content[1]).toMatchObject({
      data: { toolCallName: 'get_novel_events' },
      status: 'complete',
    });
  });

  it('keeps replayed text out of HTML rendering', () => {
    const message = createReplayMessage(7);
    reduceRunEvents(message, [
      event(1, 'delta', { text: '<b>不是标签</b>' }),
      event(2, 'completed'),
    ]);
    // 完成态的文本块会走 v-html；回放消息随后被权威历史替换，保持 streaming。
    expect(message.content[0]).toMatchObject({
      data: '<b>不是标签</b>',
      status: 'streaming',
    });
    expect(message.status).toBe('streaming');
  });

  it('pairs each tool result with the oldest pending call of the same tool', () => {
    const message = createReplayMessage(7);
    reduceRunEvents(message, [
      event(1, 'tool_call', { tool: 'run_sub_agent_script' }),
      event(2, 'tool_call', { tool: 'run_sub_agent_script' }),
      event(3, 'tool_result', { success: true, tool: 'save_scripts' }),
      event(4, 'tool_result', {
        error: '原著核实未完成',
        success: false,
        tool: 'run_sub_agent_script',
      }),
      event(5, 'tool_result', { success: true, tool: 'run_sub_agent_script' }),
    ]);
    const [first, second] = message.content;
    expect(first).toMatchObject({
      data: { result: '执行失败：原著核实未完成' },
      status: 'error',
    });
    expect(second).toMatchObject({ data: { result: '' }, status: 'complete' });
  });

  it('reports a terminal event and leaves later calls harmless', () => {
    const message = createReplayMessage(7);
    expect(reduceRunEvents(message, [event(1, 'delta', { text: 'a' })])).toBe(false);
    expect(reduceRunEvents(message, [event(2, 'failed', { error: '用户已中止' })])).toBe(true);
    expect(reduceRunEvents(message, [])).toBe(false);
  });
});

describe('createRunReplayer', () => {
  it('mounts the message first, pages through the backlog and stops at a terminal event', async () => {
    vi.useFakeTimers();
    const fullPage = Array.from({ length: 500 }, (_, index) =>
      event(index + 1, 'delta', { text: 'a' }),
    );
    const fetchEvents = vi.fn(async (_runId: number, afterId: number) => {
      if (afterId === 0) return fullPage;
      if (afterId === 500) return [event(501, 'tool_call', { tool: 'x' })];
      if (afterId === 501) return [event(502, 'completed')];
      return [];
    });
    const onUpdate = vi.fn();
    const message = createReplayMessage(7);
    const replayer = createRunReplayer({ fetchEvents, intervalMs: 1000, onUpdate });

    replayer.start(message, 7);
    // 先挂载消息，再发起首个拉取，首批事件并入时消息已在列表里。
    expect(onUpdate).toHaveBeenCalledTimes(1);
    expect(fetchEvents).toHaveBeenCalledTimes(1);
    expect(onUpdate.mock.invocationCallOrder[0]!).toBeLessThan(
      fetchEvents.mock.invocationCallOrder[0]!,
    );

    // 满页继续翻，短页结束本轮：游标依次是 0、500。
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchEvents.mock.calls.map((call) => call[1])).toEqual([0, 500]);
    expect(message.content[0]!.data).toBe('a'.repeat(500));

    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchEvents.mock.calls.map((call) => call[1])).toEqual([0, 500, 501]);
    expect(message.content[1]).toMatchObject({
      data: { toolCallName: 'x' },
      status: 'pending',
      type: 'toolcall',
    });
    expect(onUpdate).toHaveBeenCalledTimes(3);
    // 挂载回调不带新内容；之后两次同步都并入了事件。
    expect(onUpdate.mock.calls.map((call) => call[0])).toEqual([
      false,
      true,
      true,
    ]);

    // 终态之后不再轮询。
    await vi.advanceTimersByTimeAsync(10_000);
    expect(fetchEvents).toHaveBeenCalledTimes(3);
    vi.useRealTimers();
  });

  it('reports an idle poll as unchanged', async () => {
    vi.useFakeTimers();
    const fetchEvents = vi.fn(async () => [] as RunEvent[]);
    const onUpdate = vi.fn();
    const replayer = createRunReplayer({
      fetchEvents,
      intervalMs: 1000,
      onUpdate,
    });

    replayer.start(createReplayMessage(7), 7);
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(1000);
    // 没有新事件时不能让调用方以为来了新内容（那会打扰正在上翻的用户）。
    expect(onUpdate.mock.calls.map((call) => call[0])).toEqual([
      false,
      false,
      false,
    ]);
    vi.useRealTimers();
  });

  it('stops paging once a page comes back short', async () => {
    vi.useFakeTimers();
    // 模拟持续产生事件的流：每次拉取都有一页新内容，永远不会为空。
    const fetchEvents = vi.fn(async (_runId: number, afterId: number) => [
      event(afterId + 1, 'delta', { text: 'a' }),
    ]);
    const replayer = createRunReplayer({
      fetchEvents,
      intervalMs: 1000,
      onUpdate: vi.fn(),
    });

    replayer.start(createReplayMessage(7), 7);
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchEvents).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });

  it('keeps polling through transient failures and gives up after five in a row', async () => {
    vi.useFakeTimers();
    const fetchEvents = vi.fn().mockRejectedValue(new Error('network'));
    const onUpdate = vi.fn();
    const replayer = createRunReplayer({ fetchEvents, intervalMs: 1000, onUpdate });

    replayer.start(createReplayMessage(7), 7);
    await vi.advanceTimersByTimeAsync(0);
    for (let attempt = 0; attempt < 3; attempt++) {
      await vi.advanceTimersByTimeAsync(1000);
    }
    expect(fetchEvents).toHaveBeenCalledTimes(4);

    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchEvents).toHaveBeenCalledTimes(5);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(fetchEvents).toHaveBeenCalledTimes(5);
    vi.useRealTimers();
  });

  it('discards a fetch that resolves after stop', async () => {
    vi.useFakeTimers();
    let resolveFetch: (events: RunEvent[]) => void = () => {};
    const fetchEvents = vi.fn(
      () =>
        new Promise<RunEvent[]>((resolve) => {
          resolveFetch = resolve;
        }),
    );
    const onUpdate = vi.fn();
    const replayer = createRunReplayer({ fetchEvents, intervalMs: 1000, onUpdate });
    const message = createReplayMessage(7);

    replayer.start(message, 7);
    await vi.advanceTimersByTimeAsync(0);
    replayer.stop();
    resolveFetch([event(1, 'delta', { text: '迟到的内容' })]);
    await vi.advanceTimersByTimeAsync(10_000);

    expect(message.content).toHaveLength(0);
    expect(onUpdate).toHaveBeenCalledTimes(1);
    expect(fetchEvents).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });
});
