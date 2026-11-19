export function extractXmlContent(text: string, tag: string): null | string {
  const escapedTag = tag.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const regex = new RegExp(`<${escapedTag}>([\\s\\S]*?)</${escapedTag}>`, 'i');
  const match = text.match(regex);
  return match?.[1]?.trim() ?? null;
}

export function extractScriptItems(text: string): string {
  const matches = text.matchAll(
    /<scriptItem\s+name="([^"]*)">([\s\S]*?)<\/scriptItem>/gi,
  );
  const items: string[] = [];
  for (const match of matches) {
    items.push(`### ${match[1]}\n\n${match[2]?.trim() ?? ''}`);
  }
  return items.length > 0 ? items.join('\n\n---\n\n') : text;
}

export function formatEventDisplay(eventJson: string): string {
  if (!eventJson) return '';
  try {
    const events = JSON.parse(eventJson);
    if (!Array.isArray(events)) return eventJson;
    return events
      .map(
        (event: { detail?: string; name?: string }, index: number) =>
          `${index + 1}.${event.name ?? ''}：${event.detail ?? ''}`,
      )
      .join('；');
  } catch {
    return eventJson;
  }
}

export function renderMarkdown(text: string): string {
  if (!text) return '';
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/^### (.+)$/gm, '<h4>$1</h4>')
    .replace(/^## (.+)$/gm, '<h3>$1</h3>')
    .replace(/^# (.+)$/gm, '<h2>$1</h2>')
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/^- (.+)$/gm, '<li>$1</li>')
    .replace(/\n\n/g, '</p><p>')
    .replace(/\n/g, '<br>');
}

export interface ScriptAgentPlanSnapshot {
  storySkeleton?: string;
  adaptationStrategy?: string;
  script?: Array<{ content: string; id: number; name: string }>;
  /** 按集断点缓存：子 Agent 已生成但尚未 save_scripts 落库的集数。 */
  scriptEpisodes?: Record<string, string>;
}

export interface ScriptAgentWorkspaceTabs {
  storySkeleton: string;
  adaptationStrategy: string;
  script: string;
}

/**
 * 从服务端持久化的剧本工作区构造三个 tab 的恢复内容。
 *
 * 剧本 tab 优先显示已正式保存的剧本；没有保存记录时回退到按集断点缓存，
 * 让中断的流水线也能立刻看到已生成的集数（带未保存提示）。
 */
export function scriptWorkspaceTabsFromPlan(
  plan: ScriptAgentPlanSnapshot,
): ScriptAgentWorkspaceTabs {
  const savedScript = plan.script?.length
    ? plan.script
        .map((item) => `### ${item.name}\n\n${item.content}`)
        .join('\n\n---\n\n')
    : '';
  const episodeEntries = Object.entries(plan.scriptEpisodes ?? {}).filter(
    ([, content]) => content.trim().length > 0,
  );
  const script =
    savedScript ||
    (episodeEntries.length
      ? [
          '> 以下为已生成但尚未保存的剧本断点，流水线完成 save_scripts 后将正式入库。',
          '',
          ...episodeEntries.map(
            ([name, content]) => `### ${name}\n\n${content}`,
          ),
        ].join('\n\n')
      : '');
  return {
    storySkeleton: plan.storySkeleton ?? '',
    adaptationStrategy: plan.adaptationStrategy ?? '',
    script,
  };
}
