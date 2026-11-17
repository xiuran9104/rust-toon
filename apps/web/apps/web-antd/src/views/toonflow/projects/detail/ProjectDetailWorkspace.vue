<script lang="ts" setup>
import { Page } from '@vben/common-ui';

import { Alert, Button, Card, Col, Row, Select, Space, Statistic } from 'ant-design-vue';

import { computed, onMounted, ref } from 'vue';
import { getProjects } from '#/api/toonflow';
import StageNav from '../../components/StageNav.vue';
import { useProjectDetail } from './useProjectDetail';

import '../../shared/page-card.css';
import '../../styles/toon-theme.css';

const props = defineProps<{ projectId: number }>();
const view = useProjectDetail(props.projectId);
const projectOptions = ref<Array<{ value: number; label: string }>>([]);
const switchOptions = computed(() => projectOptions.value.some((item) => item.value === props.projectId) ? projectOptions.value : [{ value: props.projectId, label: view.project?.name || '当前项目' }, ...projectOptions.value]);
onMounted(async () => { try { projectOptions.value = (await getProjects()).map((item) => ({ value: item.id, label: item.name })); } catch { /* The current project remains usable. */ } });
function switchProject(value: unknown) {
  const id = Number(value);
  if (Number.isSafeInteger(id) && id > 0 && id !== props.projectId) void view.router.push({ name: 'ToonflowProjectDetail', params: { id: String(id) } });
}
</script>

<template>
  <Page auto-content-height class="toon-page">
    <div class="toonflow-detail-shell" :class="{ loading: view.loading }">
      <header class="detail-header toon-header">
        <div><h1 class="toon-title">{{ view.project?.name || '项目详情' }}</h1><p class="toon-subtitle">自动记住当前阶段、剧集和节点，下次进入继续制作。</p></div><Space wrap><Button @click="view.router.push('/toonflow/projects')">项目列表</Button><Select :value="projectId" :options="switchOptions" show-search option-filter-prop="label" aria-label="切换项目" style="width: 220px; max-width: 100%" @change="switchProject" /><Button :loading="view.loading" @click="view.loadAll">刷新</Button></Space>
      </header>

      <Alert v-if="view.loadError" type="error" show-icon :message="view.loadError" class="mb-4" /><Row :gutter="[16, 16]" class="mb-4">
        <Col :xs="12" :md="6"><Card size="small"><Statistic title="角色资产" :value="view.statistics.roleCount" /></Card></Col>
        <Col :xs="12" :md="6"><Card size="small"><Statistic title="剧本" :value="view.statistics.scriptCount" /></Card></Col>
        <Col :xs="12" :md="6"><Card size="small"><Statistic title="分镜" :value="view.statistics.storyboardCount" /></Card></Col>
        <Col :xs="12" :md="6"><Card size="small"><Statistic title="视频" :value="view.statistics.videoCount" /></Card></Col>
      </Row>

      <StageNav v-model="view.activeTab" :stages="view.stages" />
      <!-- KeepAlive keeps the Agent WebSocket sessions alive across stage
       switches: an in-flight workflow keeps streaming and the workspace tabs
       keep updating instead of the backend aborting the run on disconnect. -->
      <KeepAlive>
        <component :is="view.activePanelComponent" :context="view.panelContext" />
      </KeepAlive>
    </div>
  </Page>
</template>

<style src="../project-detail.css"></style>
