/**
 * Reattach polling for durable agent runs.
 *
 * When a WebSocket session reconnects while an agent run is still executing
 * server-side (page refresh, route change, network blip), the client learns
 * about it from the `run:active` frame and follows the run to a terminal
 * state by polling runState. State fetching is injected so the watcher stays
 * unit-testable without a real API client.
 */

export const AGENT_RUN_TERMINAL_STATES = [
  'canceled',
  'failed',
  'interrupted',
  'success',
] as const;

export type AgentRunTerminalState = (typeof AGENT_RUN_TERMINAL_STATES)[number];

export interface RunWatcherOptions {
  /** Throws on transient failures; resolves with `{ state }`. */
  fetchState: (runId: number) => Promise<{ state: string }>;
  intervalMs?: number;
  /** Give up silently after this many attempts (default ~30 minutes). */
  maxAttempts?: number;
  onTerminal: (runId: number, state: AgentRunTerminalState) => void;
}

export interface RunWatcher {
  activeRunId: number | undefined;
  dispose: () => void;
  stop: () => void;
  watch: (runId: number) => void;
}

export function isTerminalRunState(state: string): state is AgentRunTerminalState {
  return (AGENT_RUN_TERMINAL_STATES as readonly string[]).includes(state);
}

export function createRunWatcher(options: RunWatcherOptions): RunWatcher {
  const intervalMs = options.intervalMs ?? 2000;
  const maxAttempts = options.maxAttempts ?? 900;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let watchedRunId: number | undefined;
  let attempts = 0;
  let disposed = false;

  function clearTimer() {
    if (timer) clearTimeout(timer);
    timer = undefined;
  }

  function settle(runId: number, state: AgentRunTerminalState) {
    stop();
    options.onTerminal(runId, state);
  }

  async function poll(runId: number) {
    if (disposed || watchedRunId !== runId) return;
    attempts += 1;
    try {
      const run = await options.fetchState(runId);
      if (disposed || watchedRunId !== runId) return;
      if (isTerminalRunState(run.state)) {
        settle(runId, run.state);
        return;
      }
    } catch {
      // Transient failures keep polling; the attempt budget bounds the run.
    }
    if (attempts >= maxAttempts) {
      stop();
      return;
    }
    if (!disposed && watchedRunId === runId) {
      timer = setTimeout(() => void poll(runId), intervalMs);
    }
  }

  function watch(runId: number) {
    if (disposed || watchedRunId === runId) return;
    clearTimer();
    watchedRunId = runId;
    attempts = 0;
    timer = setTimeout(() => void poll(runId), 500);
  }

  function stop() {
    clearTimer();
    watchedRunId = undefined;
    attempts = 0;
  }

  function dispose() {
    disposed = true;
    stop();
  }

  return {
    get activeRunId() {
      return watchedRunId;
    },
    dispose,
    stop,
    watch,
  };
}
