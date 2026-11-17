import type { VbenFormSchema } from '#/adapter/form';
import type { VxeTableGridOptions } from '#/adapter/vxe-table';
import { CommonStatusEnum, DICT_TYPE } from '@vben/constants';
import { getDictOptions } from '@vben/hooks';

import { z } from '#/adapter/form';

import {
  AI_MODEL_TYPE_OPTIONS,
  AI_PLATFORM_OPTIONS,
  getAiModelTypeLabel,
  getAiPlatformLabel,
  getModelPresetOptions,
  getPlatformProfile,
  getPlatformTypeOptions,
} from './platforms';

/** 新增/修改的表单 */
export function useFormSchema(): VbenFormSchema[] {
  return [
    {
      component: 'Input',
      fieldName: 'id',
      dependencies: {
        triggerFields: [''],
        show: () => false,
      },
    },
    {
      fieldName: 'platform',
      label: '所属平台',
      component: 'Select',
      componentProps: () => ({
        placeholder: '请选择所属平台',
        options: [...AI_PLATFORM_OPTIONS],
        allowClear: true,
      }),
      rules: 'required',
    },
    {
      fieldName: 'type',
      label: '模型类型',
      component: 'Select',
      componentProps: (values) => {
        return {
          placeholder: '请选择模型类型',
          options: getPlatformTypeOptions(values.platform),
          allowClear: true,
        };
      },
      dependencies: {
        triggerFields: ['platform', 'id'],
      },
      rules: 'required',
    },
    {
      fieldName: 'preset',
      label: '常用模型',
      component: 'Select',
      componentProps: (values) => ({
        placeholder: '选择后自动填写模型信息',
        options: getModelPresetOptions(values.platform, values.type).map(
          (preset) => ({ label: preset.label, value: preset.model }),
        ),
        allowClear: true,
        showSearch: true,
      }),
      dependencies: {
        triggerFields: ['platform', 'type'],
        show: (values) =>
          getModelPresetOptions(values.platform, values.type).length > 0,
      },
    },
    {
      fieldName: 'key',
      label: '配置键',
      component: 'Input',
      componentProps: {
        placeholder: '例如 openai-gpt-4o',
      },
      rules: 'required',
    },
    {
      component: 'Input',
      fieldName: 'name',
      label: '模型名字',
      rules: 'required',
      componentProps: {
        placeholder: '请输入模型名字',
      },
    },
    {
      component: 'Input',
      fieldName: 'model',
      label: '模型标识',
      rules: 'required',
      componentProps: {
        placeholder: '请输入模型标识',
      },
    },
    {
      fieldName: 'url',
      label: 'API 地址',
      component: 'Input',
      componentProps: (values) => ({
        class: '!w-full',
        placeholder:
          getPlatformProfile(values.platform).url ||
          '请输入供应商提供的 API 地址',
      }),
      dependencies: {
        triggerFields: ['platform'],
      },
      rules: 'required',
    },
    {
      fieldName: 'status',
      label: '开启状态',
      component: 'RadioGroup',
      componentProps: {
        options: getDictOptions(DICT_TYPE.COMMON_STATUS, 'number'),
        buttonStyle: 'solid',
        optionType: 'button',
      },
      rules: z.number().default(CommonStatusEnum.ENABLE),
    },
    {
      fieldName: 'apiKey',
      label: 'API 密钥',
      component: 'InputPassword',
      componentProps: {
        class: '!w-full',
        placeholder: '请输入 API 密钥；本地模型可留空',
      },
    },
  ];
}

/** 列表的搜索表单 */
export function useGridFormSchema(): VbenFormSchema[] {
  return [
    {
      fieldName: 'name',
      label: '模型名字',
      component: 'Input',
      componentProps: {
        placeholder: '请输入模型名字',
        allowClear: true,
      },
    },
    {
      fieldName: 'model',
      label: '模型标识',
      component: 'Input',
      componentProps: {
        placeholder: '请输入模型标识',
        allowClear: true,
      },
    },
    {
      fieldName: 'platform',
      label: '模型平台',
      component: 'Select',
      componentProps: () => ({
        placeholder: '请选择模型平台',
        options: [...AI_PLATFORM_OPTIONS],
        allowClear: true,
        showSearch: true,
        optionFilterProp: 'label',
      }),
    },
    {
      fieldName: 'type',
      label: '模型类型',
      component: 'Select',
      componentProps: {
        placeholder: '请选择模型类型',
        options: AI_MODEL_TYPE_OPTIONS,
        allowClear: true,
      },
    },
  ];
}

/** 列表的字段 */
export function useGridColumns(): VxeTableGridOptions['columns'] {
  return [
    {
      field: 'platform',
      title: '所属平台',
      formatter: ({ cellValue }) => getAiPlatformLabel(cellValue),
      minWidth: 100,
    },
    {
      field: 'type',
      title: '模型类型',
      formatter: ({ cellValue }) => getAiModelTypeLabel(cellValue),
      minWidth: 100,
    },
    {
      field: 'name',
      title: '模型名字',
      minWidth: 180,
    },
    {
      title: '模型标识',
      field: 'model',
      minWidth: 180,
    },
    {
      title: '能力',
      field: 'capabilities',
      formatter: ({ cellValue }) =>
        Array.isArray(cellValue) ? cellValue.join(' · ') : '—',
      minWidth: 180,
    },
    {
      title: 'API 地址',
      field: 'url',
      minWidth: 140,
    },
    {
      field: 'status',
      title: '状态',
      cellRender: {
        name: 'CellDict',
        props: { type: DICT_TYPE.COMMON_STATUS },
      },
      minWidth: 80,
    },
    {
      title: '操作',
      width: 220,
      fixed: 'right',
      slots: { default: 'actions' },
    },
  ];
}
