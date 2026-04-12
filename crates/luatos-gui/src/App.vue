<script setup lang="ts">
import { ref } from 'vue'
import { useSerialStore } from './stores/serial'
import SerialBar from './components/SerialBar.vue'
import FlashView from './views/FlashView.vue'
import LogView from './views/LogView.vue'
import ProjectView from './views/ProjectView.vue'
import BuildView from './views/BuildView.vue'
import ResourceView from './views/ResourceView.vue'
import SettingsView from './views/SettingsView.vue'

type ViewId = 'flash' | 'log' | 'project' | 'build' | 'resource' | 'settings'

const activeView = ref<ViewId>('flash')
const serial = useSerialStore()

const navItems: { id: ViewId; label: string; icon: string }[] = [
  { id: 'flash',    label: '刷机',     icon: '⚡' },
  { id: 'log',      label: '日志',     icon: '📋' },
  { id: 'project',  label: '项目',     icon: '📁' },
  { id: 'build',    label: '构建',     icon: '🔨' },
  { id: 'resource', label: '固件资源', icon: '📦' },
  { id: 'settings', label: '设置',     icon: '⚙️' },
]

function switchTo(view: ViewId) {
  activeView.value = view
}
</script>

<template>
  <div class="flex flex-col h-screen bg-gray-950 text-gray-100 font-sans select-none overflow-hidden">

    <!-- 主布局 -->
    <div class="flex flex-1 overflow-hidden">

      <!-- 侧边导航 -->
      <aside class="w-40 shrink-0 flex flex-col bg-gray-900 border-r border-gray-800">
        <div class="px-4 py-4 border-b border-gray-800">
          <span class="text-lg font-bold tracking-wide text-cyan-400">LuatOS</span>
          <span class="ml-1 text-xs text-gray-500">GUI</span>
        </div>

        <nav class="flex-1 py-2 space-y-0.5 px-2">
          <button
            v-for="item in navItems"
            :key="item.id"
            @click="activeView = item.id"
            :class="[
              'w-full flex items-center gap-2.5 px-3 py-2 rounded-md text-sm transition-colors duration-100',
              activeView === item.id
                ? 'bg-cyan-600 text-white font-semibold'
                : 'text-gray-400 hover:bg-gray-800 hover:text-gray-100',
            ]"
          >
            <span class="text-base leading-none">{{ item.icon }}</span>
            <span>{{ item.label }}</span>
          </button>
        </nav>

        <!-- 串口指示器 -->
        <div class="px-3 py-2 border-t border-gray-800 text-xs text-gray-500 truncate">
          📡 {{ serial.selectedPort || '未选择' }}
        </div>
      </aside>

      <!-- 主内容 -->
      <main class="flex-1 flex flex-col overflow-hidden">
        <!-- 全局串口工具栏 -->
        <div class="shrink-0 p-3 pb-0">
          <SerialBar />
        </div>

        <!-- 视图内容 -->
        <div class="flex-1 overflow-auto p-4">
          <FlashView    v-if="activeView === 'flash'" @switch-to="(v: string) => switchTo(v as ViewId)" />
          <LogView      v-else-if="activeView === 'log'" />
          <ProjectView  v-else-if="activeView === 'project'" />
          <BuildView    v-else-if="activeView === 'build'" />
          <ResourceView v-else-if="activeView === 'resource'" />
          <SettingsView v-else-if="activeView === 'settings'" />
        </div>
      </main>
    </div>

    <!-- 状态栏 -->
    <footer class="shrink-0 flex items-center gap-4 px-4 py-1.5 bg-gray-900 border-t border-gray-800 text-xs text-gray-400">
      <span
        :class="serial.selectedPort ? 'text-green-400' : 'text-gray-600'"
        class="flex items-center gap-1.5"
      >
        <span
          :class="serial.selectedPort ? 'bg-green-400' : 'bg-gray-600'"
          class="inline-block w-2 h-2 rounded-full"
        />
        {{ serial.selectedPort || '未连接' }}
      </span>
      <span class="text-gray-600">{{ serial.baudRate }} baud</span>
      <span class="ml-auto text-gray-700">LuatOS GUI · v1.3.0</span>
    </footer>

  </div>
</template>
