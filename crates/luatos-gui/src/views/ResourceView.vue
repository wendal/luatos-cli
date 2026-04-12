<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn, type Event } from '@tauri-apps/api/event'

interface FileInfo {
  desc: string
  filename: string
  sha256: string
  size: number
  size_text: string
  path: string
}

interface VersionInfo {
  name: string
  desc: string | null
  files: FileInfo[]
}

interface ChildInfo {
  name: string
  desc: string | null
  versions: VersionInfo[]
}

interface CategoryInfo {
  name: string
  desc: string | null
  children: ChildInfo[]
}

interface ResourceProgress {
  filename: string
  downloaded: number
  total: number
  index: number
  file_count: number
  status: string
  message: string
}

const categories = ref<CategoryInfo[]>([])
const isLoading = ref(false)
const error = ref<string | null>(null)
const message = ref<string | null>(null)

// 展开状态
const expandedCats = ref<Set<string>>(new Set())
const expandedChildren = ref<Set<string>>(new Set())

// 下载状态
const isDownloading = ref(false)
const downloadProgress = ref<ResourceProgress | null>(null)

// SOC 信息
const selectedSocPath = ref('')
const socInfo = ref<any>(null)

let unlistenProgress: UnlistenFn | null = null

// 加载资源清单
async function loadManifest() {
  isLoading.value = true
  error.value = null
  try {
    categories.value = await invoke<CategoryInfo[]>('resource_list')
    message.value = `已加载 ${categories.value.length} 个模组`
  } catch (e) {
    error.value = String(e)
  } finally {
    isLoading.value = false
  }
}

// 展开/折叠分类
function toggleCat(name: string) {
  if (expandedCats.value.has(name)) {
    expandedCats.value.delete(name)
  } else {
    expandedCats.value.add(name)
  }
}

function toggleChild(key: string) {
  if (expandedChildren.value.has(key)) {
    expandedChildren.value.delete(key)
  } else {
    expandedChildren.value.add(key)
  }
}

// 下载模组资源
async function downloadModule(moduleName: string) {
  error.value = null
  message.value = null
  try {
    const outDir = await invoke<string | null>('open_folder_dialog', { title: '选择下载目录' })
    if (!outDir) return

    isDownloading.value = true
    downloadProgress.value = null

    await invoke('resource_download', {
      module: moduleName,
      versionFilter: null,
      outputDir: outDir,
    })
  } catch (e) {
    error.value = String(e)
    isDownloading.value = false
  }
}

// 取消下载
async function cancelDownload() {
  try {
    await invoke('resource_cancel')
  } catch (_) { /* 忽略 */ }
  isDownloading.value = false
}

// 查看 SOC 信息
async function viewSocInfo(filePath?: string) {
  try {
    const path = filePath || await invoke<string | null>('open_file_dialog', {
      title: '选择固件 (.soc)',
      filterName: 'LuatOS 固件',
      extensions: ['soc'],
    })
    if (!path) return
    selectedSocPath.value = path as string
    socInfo.value = await invoke('soc_info', { path: selectedSocPath.value })
  } catch (e) {
    error.value = String(e)
    socInfo.value = null
  }
}

// 监听下载进度事件
async function setupListeners() {
  unlistenProgress = await listen<ResourceProgress>('resource:progress', (evt: Event<ResourceProgress>) => {
    downloadProgress.value = evt.payload
    if (evt.payload.status === 'complete' || evt.payload.status === 'error' || evt.payload.status === 'partial') {
      isDownloading.value = false
      message.value = evt.payload.message
    }
  })
}

onMounted(async () => {
  await setupListeners()
  await loadManifest()
})

onUnmounted(() => {
  unlistenProgress?.()
})
</script>

<template>
  <div class="space-y-4">
    <div class="flex items-center justify-between">
      <h1 class="text-xl font-bold text-gray-100">📦 固件资源</h1>
      <div class="flex gap-2">
        <button @click="viewSocInfo()" class="px-3 py-1.5 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200 transition-colors">
          🔍 查看 SOC
        </button>
        <button @click="loadManifest" :disabled="isLoading"
          class="px-3 py-1.5 bg-gray-700 hover:bg-gray-600 disabled:opacity-40 rounded text-sm text-gray-200 transition-colors">
          {{ isLoading ? '加载中...' : '🔄 刷新列表' }}
        </button>
      </div>
    </div>

    <!-- 消息/错误 -->
    <p v-if="message" class="text-sm text-green-400">✅ {{ message }}</p>
    <p v-if="error" class="text-sm text-red-400">❌ {{ error }}</p>

    <!-- 下载进度 -->
    <div v-if="isDownloading && downloadProgress" class="bg-gray-900 border border-cyan-800 rounded-lg p-4 space-y-2">
      <div class="flex items-center justify-between text-sm">
        <span class="text-gray-300 font-mono truncate">{{ downloadProgress.filename }}</span>
        <button @click="cancelDownload" class="px-2 py-1 bg-red-700 hover:bg-red-600 rounded text-xs text-white">取消</button>
      </div>
      <div v-if="downloadProgress.total > 0" class="w-full bg-gray-800 rounded-full h-2">
        <div class="bg-cyan-500 h-2 rounded-full transition-all"
          :style="{ width: Math.min(100, downloadProgress.downloaded / downloadProgress.total * 100) + '%' }"></div>
      </div>
      <p class="text-xs text-gray-500">{{ downloadProgress.message }}</p>
    </div>

    <!-- SOC 信息面板 -->
    <div v-if="socInfo" class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-2">
      <div class="flex items-center justify-between">
        <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">SOC 信息</h2>
        <button @click="socInfo = null" class="text-gray-600 hover:text-gray-400 text-sm">✕</button>
      </div>
      <p class="text-xs text-gray-500 truncate">{{ selectedSocPath }}</p>
      <pre class="text-xs text-gray-300 bg-gray-950 rounded p-2 overflow-x-auto">{{ JSON.stringify(socInfo, null, 2) }}</pre>
    </div>

    <!-- 资源列表 -->
    <div v-if="categories.length > 0" class="space-y-2">
      <div v-for="cat in categories" :key="cat.name" class="bg-gray-900 border border-gray-800 rounded-lg overflow-hidden">

        <!-- 分类标题 -->
        <div @click="toggleCat(cat.name)"
          class="flex items-center justify-between px-4 py-3 cursor-pointer hover:bg-gray-800 transition-colors">
          <div class="flex items-center gap-3">
            <span class="text-gray-600 text-xs">{{ expandedCats.has(cat.name) ? '▼' : '▶' }}</span>
            <span class="text-sm font-semibold text-gray-200">{{ cat.name }}</span>
            <span v-if="cat.desc" class="text-xs text-gray-500">{{ cat.desc }}</span>
          </div>
          <button @click.stop="downloadModule(cat.name)" :disabled="isDownloading"
            class="px-3 py-1 bg-cyan-700 hover:bg-cyan-600 disabled:opacity-40 rounded text-xs text-white transition-colors">
            ⬇ 下载
          </button>
        </div>

        <!-- 子项 -->
        <div v-if="expandedCats.has(cat.name)" class="border-t border-gray-800">
          <div v-for="child in cat.children" :key="child.name" class="border-b border-gray-800/50 last:border-b-0">
            <div @click="toggleChild(cat.name + '/' + child.name)"
              class="flex items-center gap-3 px-6 py-2 cursor-pointer hover:bg-gray-850 transition-colors">
              <span class="text-gray-600 text-xs">{{ expandedChildren.has(cat.name + '/' + child.name) ? '▼' : '▶' }}</span>
              <span class="text-sm text-gray-300">{{ child.name }}</span>
              <span v-if="child.desc" class="text-xs text-gray-600">{{ child.desc }}</span>
            </div>

            <!-- 版本列表 -->
            <div v-if="expandedChildren.has(cat.name + '/' + child.name)" class="px-8 pb-2 space-y-1">
              <div v-for="ver in child.versions" :key="ver.name" class="bg-gray-850 rounded px-3 py-2">
                <div class="text-xs font-semibold text-cyan-400 mb-1">{{ ver.name }}
                  <span v-if="ver.desc" class="text-gray-600 font-normal ml-2">{{ ver.desc }}</span>
                </div>
                <div v-for="f in ver.files" :key="f.filename"
                  class="flex items-center gap-3 text-xs text-gray-400 py-0.5 hover:text-gray-300">
                  <span class="font-mono truncate flex-1">{{ f.filename }}</span>
                  <span class="text-gray-600 shrink-0">{{ f.size_text }}</span>
                  <span class="text-gray-700 truncate max-w-[200px]" :title="f.desc">{{ f.desc }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 空状态 -->
    <div v-if="categories.length === 0 && !isLoading && !error" class="bg-gray-900 border border-gray-800 rounded-lg p-8 text-center">
      <p class="text-gray-500 text-sm">点击"刷新列表"从 CDN 获取固件资源</p>
    </div>
  </div>
</template>
