import { describe, expect, it } from 'vitest';

import {
  extractScriptItems,
  extractXmlContent,
  formatEventDisplay,
  renderMarkdown,
  scriptWorkspaceTabsFromPlan,
} from './production-content';

describe('production content helpers', () => {
  it('extracts agent XML sections and script items', () => {
    expect(extractXmlContent('<plan>  outline  </plan>', 'plan')).toBe('outline');
    expect(
      extractScriptItems('<scriptItem name="第一集">场景一</scriptItem>'),
    ).toBe('### 第一集\n\n场景一');
  });

  it('keeps invalid event payloads and formats valid arrays', () => {
    expect(formatEventDisplay('not-json')).toBe('not-json');
    expect(formatEventDisplay('[{"name":"相遇","detail":"车站"}]')).toBe(
      '1.相遇：车站',
    );
  });

  it('escapes HTML before rendering the supported markdown subset', () => {
    expect(renderMarkdown('# 标题\n\n**内容** <script>')).toContain(
      '<h2>标题</h2>',
    );
    expect(renderMarkdown('<script>')).toBe('&lt;script&gt;');
  });
});

describe('scriptWorkspaceTabsFromPlan', () => {
  it('prefers saved scripts and keeps skeleton/strategy verbatim', () => {
    const tabs = scriptWorkspaceTabsFromPlan({
      storySkeleton: '骨架',
      adaptationStrategy: '策略',
      script: [
        { id: 2, name: '第1集', content: '△开场' },
        { id: 3, name: '第2集', content: '△高潮' },
      ],
      scriptEpisodes: { 第1集: '旧断点' },
    });
    expect(tabs.storySkeleton).toBe('骨架');
    expect(tabs.adaptationStrategy).toBe('策略');
    expect(tabs.script).toBe('### 第1集\n\n△开场\n\n---\n\n### 第2集\n\n△高潮');
  });

  it('falls back to episode checkpoints with an unsaved hint', () => {
    const tabs = scriptWorkspaceTabsFromPlan({
      script: [],
      scriptEpisodes: { 第1集: '△第一集', 第2集: '  ' },
    });
    expect(tabs.script).toContain('尚未保存');
    expect(tabs.script).toContain('### 第1集\n\n△第一集');
    expect(tabs.script).not.toContain('第2集');
  });

  it('returns empty tabs for an empty plan', () => {
    expect(scriptWorkspaceTabsFromPlan({})).toEqual({
      storySkeleton: '',
      adaptationStrategy: '',
      script: '',
    });
  });
});
