import { createApp, defineComponent, h, nextTick } from 'vue';
import { afterEach, describe, expect, it, vi } from 'vitest';

import ProductionAssetStrip from './ProductionAssetStrip.vue';

const api = vi.hoisted(() => ({
  executeAgentTool: vi.fn(),
  polishAssetPrompt: vi.fn(),
  saveAsset: vi.fn(),
}));
const success = vi.hoisted(() => vi.fn());

vi.mock('#/api/toonflow', () => api);
vi.mock('../assets/asset-types', () => ({
  assetFileUrl: (path: string) => path,
  assetTypeLabel: () => '角色',
}));
vi.mock('ant-design-vue', () => {
  const box = defineComponent({
    setup:
      (_, { slots }) =>
      () =>
        h('div', slots.default?.()),
  });
  const textArea = defineComponent({
    props: ['value'],
    emits: ['update:value'],
    setup:
      (props, { emit }) =>
      () =>
        h('textarea', {
          value: props.value,
          onInput: (event: Event) =>
            emit('update:value', (event.target as HTMLTextAreaElement).value),
        }),
  });
  const modal = defineComponent({
    props: ['open'],
    setup:
      (props, { slots }) =>
      () =>
        props.open ? h('div', [slots.default?.(), slots.footer?.()]) : null,
  });
  const select = defineComponent({
    props: ['value', 'options'],
    setup:
      (props) =>
      () =>
        h('select', { 'aria-label': '造型状态' }, (props.options ?? []).map(() => null)),
  });
  const button = defineComponent({
    props: ['loading', 'disabled'],
    setup:
      (props, { slots }) =>
      () =>
        h('button', { disabled: props.disabled || props.loading }, slots.default?.()),
  });
  const dropdown = defineComponent({
    setup:
      (_, { slots }) =>
      () =>
        h('div', [slots.default?.(), slots.overlay?.()]),
  });
  return {
    Button: button,
    Dropdown: dropdown,
    Empty: Object.assign({ ...box }, { PRESENTED_IMAGE_SIMPLE: 'simple' }),
    Form: Object.assign({ ...box }, { Item: box }),
    Input: { TextArea: textArea },
    Menu: Object.assign({ ...box }, { Item: button }),
    Modal: modal,
    Select: select,
    Tag: box,
    message: { error: vi.fn(), success, warning: vi.fn() },
  };
});

let app: ReturnType<typeof createApp>;
let host: HTMLDivElement;

afterEach(() => {
  app?.unmount();
  host?.remove();
  vi.clearAllMocks();
});

async function flush() {
  for (let index = 0; index < 8; index += 1) await nextTick();
}

describe('production derived asset prompts', () => {
  it('lets a user edit and persist the prompt before direct generation', async () => {
    api.saveAsset.mockResolvedValue({ id: 2 });
    const refresh = vi.fn();
    const derived = {
      id: 2,
      projectId: 1,
      parentAssetId: 1,
      scriptId: 10,
      name: '雨夜换装',
      type: 'role',
      description: '深色雨衣',
      prompt: '旧提示词',
    };
    const assets = [
      {
        id: 1,
        projectId: 1,
        name: '角色甲',
        type: 'role',
        description: '基础角色',
        prompt: '',
        derive: [derived],
      },
    ];

    host = document.createElement('div');
    document.body.append(host);
    app = createApp(
      defineComponent({
        setup: () => () =>
          h(ProductionAssetStrip, { assets, onRefresh: refresh }),
      }),
    );
    app.mount(host);
    await flush();

    const editButton = [...host.querySelectorAll('button')].find(
      (button) => button.textContent?.trim() === '编辑造型',
    )!;
    editButton.click();
    await flush();

    const [description, prompt] = [...host.querySelectorAll('textarea')];
    description!.value = '人工调整后的造型描述';
    description!.dispatchEvent(new Event('input'));
    prompt!.value = '人工调整后的生成提示词';
    prompt!.dispatchEvent(new Event('input'));
    [...host.querySelectorAll('button')]
      .find((button) => button.textContent?.trim() === '保存造型')!
      .click();
    await flush();

    expect(api.saveAsset).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 2,
        parentAssetId: 1,
        description: '人工调整后的造型描述',
        prompt: '人工调整后的生成提示词',
      }),
    );
    expect(success).toHaveBeenCalledWith('“雨夜换装”的造型已保存');
    expect(refresh).toHaveBeenCalledOnce();
  });

  it('materializes a pending appearance before editing its description and prompt', async () => {
    api.executeAgentTool.mockImplementation(async ({ toolName }) =>
      toolName === 'add_deriveAsset' ? { result: { id: 3 } } : { result: true },
    );
    api.saveAsset.mockResolvedValue({ id: 3 });
    const refresh = vi.fn();
    const assets = [{
      id: 1,
      projectId: undefined as unknown as number,
      name: '角色甲',
      type: 'role',
      description: '基础角色',
      prompt: '',
      imageFilePath: '/role.png',
      derive: [],
      appearances: [{
        id: 2,
        roleAssetId: 1,
        name: '夜行造型',
        scenes: ['场1'],
        costumePrompt: '黑色夜行衣',
        description: '潜入时使用',
      }],
    }];

    host = document.createElement('div');
    document.body.append(host);
    app = createApp(defineComponent({
      setup: () => () => h(ProductionAssetStrip, {
        assets,
        script: {
          id: 10,
          name: '第1集',
          content: '',
          projectId: 100,
          createTime: 0,
          relatedAssets: [],
        },
        onRefresh: refresh,
      }),
    }));
    app.mount(host);
    await flush();

    const editButton = [...host.querySelectorAll('button')].find((button) =>
      button.textContent?.trim() === '编辑造型',
    )!;
    editButton.click();
    await flush();

    expect(api.executeAgentTool).toHaveBeenCalledWith(expect.objectContaining({
      projectId: 100,
      scriptId: 10,
      toolName: 'add_deriveAsset',
      arguments: expect.objectContaining({ assetsId: 1, appearanceId: 2 }),
    }));

    const [description, prompt] = [...host.querySelectorAll('textarea')];
    expect(description!.value).toBe('黑色夜行衣');
    expect(prompt!.value).toBe('黑色夜行衣');
    description!.value = '潜入用黑色夜行造型';
    description!.dispatchEvent(new Event('input'));
    prompt!.value = '黑色夜行衣，正面全身立绘';
    prompt!.dispatchEvent(new Event('input'));
    [...host.querySelectorAll('button')]
      .find((button) => button.textContent?.trim() === '保存造型')!
      .click();
    await flush();

    expect(api.saveAsset).toHaveBeenCalledWith(expect.objectContaining({
      id: 3,
      parentAssetId: 1,
      projectId: 100,
      description: '潜入用黑色夜行造型',
      prompt: '黑色夜行衣，正面全身立绘',
    }));
    expect(refresh).toHaveBeenCalled();
  });

  it('supports AI prompts and image generation for pending and created items', async () => {
    api.executeAgentTool.mockImplementation(async ({ toolName }) =>
      toolName === 'add_deriveAsset' ? { result: { id: 3 } } : { result: true },
    );
    api.polishAssetPrompt.mockResolvedValue({ assetsId: 3, prompt: 'AI 优化提示词' });
    const refresh = vi.fn();
    const edit = vi.fn();
    const derived = {
      id: 4,
      projectId: undefined as unknown as number,
      parentAssetId: 1,
      scriptId: 10,
      name: '已创建造型',
      type: 'role',
      description: '都市西装',
      prompt: '都市西装提示词',
      imageFilePath: '/derived.png',
    };
    const assets = [{
      id: 1,
      projectId: undefined as unknown as number,
      name: '角色甲',
      type: 'role',
      description: '基础角色',
      prompt: '',
      imageFilePath: '/role.png',
      derive: [derived],
      appearances: [{
        id: 2,
        roleAssetId: 1,
        name: '夜行造型',
        scenes: ['场1'],
        costumePrompt: '黑色夜行衣',
        description: '潜入时使用',
      }],
    }];

    host = document.createElement('div');
    document.body.append(host);
    app = createApp(defineComponent({
      setup: () => () => h(ProductionAssetStrip, {
        assets,
        script: {
          id: 10,
          name: '第1集',
          content: '',
          projectId: 100,
          createTime: 0,
          relatedAssets: [],
        },
        onEdit: edit,
        onRefresh: refresh,
      }),
    }));
    app.mount(host);
    await flush();

    const buttons = () => [...host.querySelectorAll('button')];
    const aiButtons = buttons().filter(
      (button) => button.textContent?.trim() === 'AI 生成提示词',
    );
    expect(aiButtons).toHaveLength(2);
    aiButtons[0]!.click();
    await flush();
    expect(api.polishAssetPrompt).toHaveBeenCalledWith(expect.objectContaining({
      assetsId: 4,
      describe: '都市西装',
      projectId: 100,
    }));

    buttons().find((button) => button.textContent?.trim() === '重新生成')!.click();
    await flush();
    buttons().find((button) => button.textContent?.trim() === '图片编辑')!.click();
    expect(api.executeAgentTool).toHaveBeenCalledWith(expect.objectContaining({
      projectId: 100,
      scriptId: 10,
      toolName: 'generate_deriveAsset',
      arguments: { ids: [4], concurrentCount: 1 },
    }));
    expect(edit).toHaveBeenCalledWith(derived);

    aiButtons[1]!.click();
    await flush();
    expect(api.executeAgentTool).toHaveBeenCalledWith(expect.objectContaining({
      toolName: 'add_deriveAsset',
      arguments: expect.objectContaining({ assetsId: 1, appearanceId: 2 }),
    }));
    expect(api.polishAssetPrompt).toHaveBeenCalledWith(expect.objectContaining({
      assetsId: 3,
      describe: '黑色夜行衣',
    }));

    api.executeAgentTool.mockClear();
    buttons().find((button) => button.textContent?.trim() === '生成图片')!.click();
    await flush();
    expect(api.executeAgentTool).toHaveBeenNthCalledWith(1, expect.objectContaining({
      projectId: 100,
      toolName: 'add_deriveAsset',
    }));
    expect(api.executeAgentTool).toHaveBeenNthCalledWith(2, expect.objectContaining({
      projectId: 100,
      toolName: 'generate_deriveAsset',
      arguments: { ids: [3], concurrentCount: 1 },
    }));
  });

  it('locks an image button immediately while generation is pending', async () => {
    let finishGeneration!: (value: { result: boolean }) => void;
    api.executeAgentTool.mockReturnValue(
      new Promise((resolve) => {
        finishGeneration = resolve;
      }),
    );
    const derived = {
      id: 4,
      projectId: undefined as unknown as number,
      parentAssetId: 1,
      scriptId: 10,
      name: '已创建造型',
      type: 'role',
      description: '都市西装',
      prompt: '都市西装提示词',
    };
    const assets = [{
      id: 1,
      projectId: undefined as unknown as number,
      name: '角色甲',
      type: 'role',
      description: '基础角色',
      prompt: '',
      imageFilePath: '/role.png',
      derive: [derived],
      appearances: [],
    }];

    host = document.createElement('div');
    document.body.append(host);
    app = createApp(defineComponent({
      setup: () => () => h(ProductionAssetStrip, {
        assets,
        script: {
          id: 10,
          name: '第1集',
          content: '',
          projectId: 100,
          createTime: 0,
          relatedAssets: [],
        },
      }),
    }));
    app.mount(host);
    await flush();

    const generateButton = [...host.querySelectorAll('button')].find(
      (button) => button.textContent?.trim() === '生成图片',
    )!;
    generateButton.click();
    await flush();
    expect(generateButton.disabled).toBe(true);
    expect(generateButton.textContent?.trim()).toBe('生成中');
    generateButton.click();
    expect(api.executeAgentTool).toHaveBeenCalledOnce();

    finishGeneration({ result: true });
    await flush();
    expect(generateButton.disabled).toBe(false);
    expect(generateButton.textContent?.trim()).toBe('生成图片');
  });
});
