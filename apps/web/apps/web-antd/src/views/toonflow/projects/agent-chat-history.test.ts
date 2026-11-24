import type { AgentChatMessage } from './agent-chat-history';

import { describe, expect, it } from 'vitest';

import {
  historyContentPreview,
  mergeAgentHistory,
} from './agent-chat-history';
import { createReplayMessage } from './agent-run-replay';

function message(
  id: string,
  status = 'complete',
  historical = true,
): AgentChatMessage {
  return {
    id,
    role: 'assistant',
    status,
    datetime: '2026-01-01T00:00:00Z',
    historical,
    content: [],
  };
}

describe('agent chat history', () => {
  it('replaces a restored snapshot without duplicating completed messages', () => {
    const pending = message('live', 'streaming', false);
    expect(
      mergeAgentHistory(
        [message('legacy-duplicate'), pending],
        [message('memory:1'), message('memory:2')],
        false,
      ).map((item) => item.id),
    ).toEqual(['memory:1', 'memory:2', 'live']);
  });

  it('prepends older pages once using stable memory ids', () => {
    expect(
      mergeAgentHistory(
        [message('memory:2'), message('memory:3')],
        [message('memory:1'), message('memory:2')],
        true,
      ).map((item) => item.id),
    ).toEqual(['memory:1', 'memory:2', 'memory:3']);
  });

  it('collapses large historical output until explicitly expanded', () => {
    const text = 'a'.repeat(20);
    expect(historyContentPreview(text, false, 8)).toEqual({
      text: 'aaaaaaaa\n…',
      truncated: true,
    });
    expect(historyContentPreview(text, true, 8).text).toBe(text);
  });

  it('keeps a replay message when a snapshot or an older page arrives', () => {
    // 回放消息住在同一个消息数组里，靠的是“在途消息会被保留”这条规则。
    const replay = createReplayMessage(7);
    expect(
      mergeAgentHistory([replay], [message('memory:1')], false).map(
        (item) => item.id,
      ),
    ).toEqual(['memory:1', 'run:7:replay']);
    expect(
      mergeAgentHistory([replay], [message('memory:0')], true).map(
        (item) => item.id,
      ),
    ).toEqual(['memory:0', 'run:7:replay']);
  });
});
