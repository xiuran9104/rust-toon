<script setup lang="ts">
/* eslint-disable vue/no-mutating-props, vue/no-v-html -- context is a shared view model and HTML is produced by the existing renderer */
import { onMounted } from 'vue';

import { Button, Card, Col, Popconfirm, Row, Space, Tabs } from 'ant-design-vue';

import AgentChat from '../AgentChat.vue';
const props = defineProps<{ context: any }>();
// Refill the workspace tabs from the persisted script workspace on every
// mount, independent of the loadAll timing.
onMounted(() => void props.context.restoreScriptWorkspace?.());
</script>
<template><section class="detail-panel detail-panel--chat"><Row :gutter="16"><Col :span="8"><AgentChat :ref="context.setAgentChatRef" agent-type="scriptAgent" :project-id="context.projectId" :messages="context.scriptChatMessages" @clear-memory="context.clearScriptAgentMemory" @tool-result="context.onAgentToolResult" /></Col><Col :span="16"><Card class="workspace-card" size="small" title="剧本创作工作区"><template #extra><Space><Popconfirm title="确认清除剧本 Agent 的对话和工作区？" @confirm="context.resetAgentWorkspace"><Button>重新开始</Button></Popconfirm><Button @click="context.openScriptGeneration">开始创作</Button><Button type="primary" @click="context.saveAgentWorkspace">保存工作区</Button></Space></template><Tabs v-model:active-key="context.workspaceActiveTab" size="small"><Tabs.TabPane v-for="tab in context.workspaceTabs" :key="tab.key" :tab="tab.label"><div v-if="!tab.content" class="workspace-placeholder">剧本 Agent 将在此展示{{ tab.label }}...</div><div v-else class="workspace-output" v-html="context.renderMarkdown(tab.content)"></div></Tabs.TabPane></Tabs></Card></Col></Row></section></template>
