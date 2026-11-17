<script lang="ts" setup>
import type { AiModelModelApi } from '#/api/ai/model/model';

import { computed, ref } from 'vue';

import { useVbenModal } from '@vben/common-ui';

import { message } from 'ant-design-vue';

import { useVbenForm } from '#/adapter/form';
import {
  createModel,
  discoverModels,
  getModel,
  updateModel,
} from '#/api/ai/model/model';
import { getModelPlatformCapabilities } from '#/api/ai/model/model';
import { $t } from '#/locales';

import { useFormSchema } from '../data';
import {
  applyDiscoveredModels,
  applyGatewayCapabilities,
  getAiPlatformLabel,
  getModelPresetOptions,
  getPlatformProfile,
  getPlatformTypeOptions,
  modelConfigKey,
  shouldApplyPlatformDefaultUrl,
} from '../platforms';

const emit = defineEmits(['success']);
const formData = ref<AiModelModelApi.Model>();
const previousPlatform = ref('');
const previousType = ref('');
const previousPreset = ref('');
const previousPlatformUrl = ref('');
let discoverTimer: ReturnType<typeof setTimeout> | undefined;
let lastDiscoveryKey = '';
const getTitle = computed(() => {
  return formData.value?.id
    ? $t('ui.actionTitle.edit', ['模型配置'])
    : $t('ui.actionTitle.create', ['模型配置']);
});

const [Form, formApi] = useVbenForm({
  commonConfig: {
    componentProps: {
      class: 'w-full',
    },
    formItemClass: 'col-span-2',
    labelWidth: 120,
  },
  layout: 'horizontal',
  schema: useFormSchema(),
  showDefaultActions: false,
  handleValuesChange: async (values) => {
    const platform = String(values.platform || '');
    const type = String(values.type || '');
    const preset = String(values.preset || '');
    const url = String(values.url || '');
    const apiKey = String(values.apiKey || '');
    if (platform && platform !== previousPlatform.value) {
      const platformDefaultUrl = getPlatformProfile(platform).url;
      // 切换平台时只在地址仍是上一个平台的默认值（或为空）时套用新默认地址，
      // 手动填写的地址（例如指向自建 OpenAI 兼容服务）必须保留。
      if (
        shouldApplyPlatformDefaultUrl(url, previousPlatformUrl.value) &&
        platformDefaultUrl
      ) {
        await formApi.setFieldValue('url', platformDefaultUrl);
      }
      previousPlatformUrl.value = platformDefaultUrl;
      previousPlatform.value = platform;
      const supportedTypes = getPlatformTypeOptions(platform);
      await formApi.setFieldValue('preset', undefined);
      if (!supportedTypes.some((option) => option.value === type)) {
        await formApi.setFieldValue('type', supportedTypes[0]?.value);
      }
    }
    if (type && type !== previousType.value) {
      previousType.value = type;
      previousPreset.value = '';
      await formApi.setFieldValue('preset', undefined);
    }
    if (preset && preset !== previousPreset.value) {
      previousPreset.value = preset;
      const selected = getModelPresetOptions(platform, type).find(
        (item) => item.model === preset,
      );
      await formApi.setFieldValue('model', preset);
      await formApi.setFieldValue(
        'name',
        `${getAiPlatformLabel(platform)} · ${selected?.label || preset}`,
      );
      await formApi.setFieldValue('key', modelConfigKey(platform, preset));
    }
    const discoveryKey = `${platform}:${url}:${apiKey}`;
    if (
      platform &&
      url &&
      discoveryKey !== lastDiscoveryKey &&
      (apiKey || platform === 'Ollama')
    ) {
      clearTimeout(discoverTimer);
      discoverTimer = setTimeout(async () => {
        lastDiscoveryKey = discoveryKey;
        try {
          const result = await discoverModels({ apiKey, platform, url });
          applyDiscoveredModels(platform, result.models);
          const currentValues = await formApi.getValues();
          formApi.setState({ schema: useFormSchema() });
          await formApi.setValues(currentValues);
          message.success(
            `已从 ${platform} 同步 ${result.models.length} 个账号可用模型`,
          );
        } catch (error) {
          lastDiscoveryKey = '';
          message.warning(
            error instanceof Error
              ? `模型同步失败：${error.message}`
              : '模型同步失败，请检查 API 地址和密钥',
          );
        }
      }, 800);
    }
  },
});

const [Modal, modalApi] = useVbenModal({
  async onConfirm() {
    const { valid } = await formApi.validate();
    if (!valid) {
      return;
    }
    modalApi.lock();
    // 提交表单
    const data = (await formApi.getValues()) as AiModelModelApi.Model;
    try {
      await (formData.value?.id ? updateModel(data) : createModel(data));
      // 关闭并提示
      await modalApi.close();
      emit('success');
      message.success($t('ui.actionMessage.operationSuccess'));
    } finally {
      modalApi.unlock();
    }
  },
  async onOpenChange(isOpen: boolean) {
    if (!isOpen) {
      formData.value = undefined;
      previousPlatform.value = '';
      previousType.value = '';
      previousPreset.value = '';
      previousPlatformUrl.value = '';
      lastDiscoveryKey = '';
      clearTimeout(discoverTimer);
      return;
    }
    const capabilities = await getModelPlatformCapabilities();
    applyGatewayCapabilities(capabilities.platforms);
    for (const platform of new Set(
      capabilities.cachedModels.map((item) => item.platform),
    )) {
      applyDiscoveredModels(
        platform,
        capabilities.cachedModels
          .filter((item) => item.platform === platform)
          .map((item) => ({ id: item.model, type: item.type })),
      );
    }
    formApi.setState({ schema: useFormSchema() });
    // 加载数据
    const data = modalApi.getData<AiModelModelApi.Model>();
    if (!data || !data.id) {
      return;
    }
    modalApi.lock();
    try {
      formData.value = await getModel(data.id);
      // 设置到 values
      await formApi.setValues(formData.value);
    } finally {
      modalApi.unlock();
    }
  },
});
</script>

<template>
  <Modal :title="getTitle" class="w-2/5">
    <Form class="mx-4" />
  </Modal>
</template>
