export const AGENT_HISTORY_PAGE_SIZE = 24;
export const AGENT_HISTORY_PREVIEW_LENGTH = 4_000;

export interface AgentChatContentBlock {
  data: any;
  id: string;
  status: string;
  type: string;
}

export interface AgentChatMessage {
  content: AgentChatContentBlock[];
  datetime: string;
  /** `replay` marks a message rebuilt from persisted run events after a reconnect. */
  ext?: { error?: string; replay?: boolean };
  historical?: boolean;
  id: string;
  name?: string;
  role: 'assistant' | 'system' | 'user';
  status: string;
}

export interface AgentHistoryFrame {
  hasMore: boolean;
  messages: AgentChatMessage[];
  oldestId?: number;
  prepend: boolean;
}

function isActiveMessage(message: AgentChatMessage) {
  return message.status === 'pending' || message.status === 'streaming';
}

export function mergeAgentHistory(
  current: AgentChatMessage[],
  history: AgentChatMessage[],
  prepend: boolean,
) {
  const incomingIds = new Set(history.map((message) => message.id));
  if (prepend) {
    const currentIds = new Set(current.map((message) => message.id));
    return [
      ...history.filter((message) => !currentIds.has(message.id)),
      ...current,
    ];
  }

  // A reconnect should replace the previous snapshot instead of appending a
  // second copy. Preserve only an in-flight bubble that may not be durable yet.
  return [
    ...history,
    ...current.filter(
      (message) =>
        isActiveMessage(message) && !incomingIds.has(message.id),
    ),
  ];
}

function contentText(value: unknown) {
  if (typeof value === 'string') return value;
  if (value === undefined || value === null) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function historyContentPreview(
  value: unknown,
  expanded: boolean,
  maxLength = AGENT_HISTORY_PREVIEW_LENGTH,
) {
  const text = contentText(value);
  const truncated = text.length > maxLength;
  return {
    text: truncated && !expanded ? `${text.slice(0, maxLength)}\n…` : text,
    truncated,
  };
}
