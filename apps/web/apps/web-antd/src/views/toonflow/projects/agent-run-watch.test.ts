import { describe, expect, it, vi } from 'vitest';

import { createRunWatcher, isTerminalRunState } from './agent-run-watch';

describe('agent run watcher', () => {
  it('recognizes terminal states only', () => {
    expect(isTerminalRunState('success')).toBe(true);
    expect(isTerminalRunState('interrupted')).toBe(true);
    expect(isTerminalRunState('running')).toBe(false);
    expect(isTerminalRunState('pending')).toBe(false);
  });

  it('polls until a terminal state and reports it once', async () => {
    vi.useFakeTimers();
    const states = ['running', 'running', 'success'];
    let calls = 0;
    const onTerminal = vi.fn();
    const watcher = createRunWatcher({
      fetchState: () => Promise.resolve({ state: states[calls++] ?? 'success' }),
      intervalMs: 2000,
      onTerminal,
    });
    watcher.watch(7);
    await vi.advanceTimersByTimeAsync(500);
    await vi.advanceTimersByTimeAsync(2000);
    await vi.advanceTimersByTimeAsync(2000);
    expect(onTerminal).toHaveBeenCalledTimes(1);
    expect(onTerminal).toHaveBeenCalledWith(7, 'success');
    expect(watcher.activeRunId).toBeUndefined();
    vi.useRealTimers();
  });

  it('keeps polling through transient fetch failures', async () => {
    vi.useFakeTimers();
    let calls = 0;
    const onTerminal = vi.fn();
    const watcher = createRunWatcher({
      fetchState: () => {
        calls += 1;
        if (calls === 1) return Promise.reject(new Error('network'));
        return Promise.resolve({ state: 'failed' });
      },
      intervalMs: 1000,
      onTerminal,
    });
    watcher.watch(9);
    await vi.advanceTimersByTimeAsync(500);
    await vi.advanceTimersByTimeAsync(1000);
    expect(onTerminal).toHaveBeenCalledWith(9, 'failed');
    vi.useRealTimers();
  });

  it('stops silently after the attempt budget', async () => {
    vi.useFakeTimers();
    const onTerminal = vi.fn();
    const watcher = createRunWatcher({
      fetchState: () => Promise.resolve({ state: 'running' }),
      intervalMs: 10,
      maxAttempts: 3,
      onTerminal,
    });
    watcher.watch(11);
    // 初始 500ms 首查 + 每轮 10ms：推进到远超预算的时间。
    await vi.advanceTimersByTimeAsync(600);
    expect(onTerminal).not.toHaveBeenCalled();
    expect(watcher.activeRunId).toBeUndefined();
    vi.useRealTimers();
  });

  it('switching targets drops the previous run and dispose cancels polling', async () => {
    vi.useFakeTimers();
    const onTerminal = vi.fn();
    const watcher = createRunWatcher({
      fetchState: () => Promise.resolve({ state: 'running' }),
      intervalMs: 1000,
      onTerminal,
    });
    watcher.watch(1);
    watcher.watch(2);
    expect(watcher.activeRunId).toBe(2);
    watcher.dispose();
    await vi.advanceTimersByTimeAsync(5000);
    expect(onTerminal).not.toHaveBeenCalled();
    vi.useRealTimers();
  });
});
