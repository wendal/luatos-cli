<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'

interface ProjectInfo {
  name: string
  chip: string
  version: string
  description: string | null
  script_dirs: string[]
  script_files: string[]
  output_dir: string
  use_luac: boolean
  bitw: number
  luac_debug: boolean
  ignore_deps: boolean
  soc_file: string | null
  port: string | null
  baud: number | null
}

interface RecentProject {
  path: string
  name: string
  chip: string
}

const project = ref<ProjectInfo | null>(null)
const projectDir = ref('')
const error = ref<string | null>(null)
const message = ref<string | null>(null)
const isDirty = ref(false)
const isSaving = ref(false)

// 最近项目
const recentProjects = ref<RecentProject[]>([])

// 新建项目对话
const showNewDialog = ref(false)
const newName = ref('my-project')
const newChip = ref('bk72xx')

// 新增脚本目录
const newScriptDir = ref('')

const chips = ['bk72xx', 'air6208', 'air101', 'air103', 'air601', 'air1601', 'ec7xx', 'air8000']

// 标记字段变化
function markDirty() {
  isDirty.value = true
}

// 加载最近项目列表
async function loadRecentProjects() {
  try {
    const settings = await invoke<any>('settings_load')
    if (settings?.recent_projects) {
      recentProjects.value = settings.recent_projects
    }
  } catch (_) { /* 忽略 */ }
}

// 保存最近项目到设置
async function saveRecentProject(dir: string, name: string, chip: string) {
  try {
    const settings = await invoke<any>('settings_load')
    const recents: RecentProject[] = settings?.recent_projects || []

    // 移除重复项
    const filtered = recents.filter((r: RecentProject) => r.path !== dir)
    // 添加到头部
    filtered.unshift({ path: dir, name, chip })
    // 最多保留 10 个
    if (filtered.length > 10) filtered.length = 10

    settings.recent_projects = filtered
    await invoke('settings_save', { settings })
    recentProjects.value = filtered
  } catch (_) { /* 忽略 */ }
}

// 选择项目目录
async function browseProject() {
  try {
    const path = await invoke<string | null>('open_folder_dialog', { title: '选择项目目录' })
    if (path) {
      projectDir.value = path
      await loadProject()
    }
  } catch (e) {
    error.value = String(e)
  }
}

// 加载项目
async function loadProject() {
  error.value = null
  message.value = null
  isDirty.value = false
  try {
    project.value = await invoke<ProjectInfo>('project_open', { dir: projectDir.value })
    message.value = `已加载项目: ${project.value.name}`
    await saveRecentProject(projectDir.value, project.value.name, project.value.chip)
  } catch (e) {
    error.value = String(e)
    project.value = null
  }
}

// 切换到最近项目
async function switchToRecent(recent: RecentProject) {
  projectDir.value = recent.path
  await loadProject()
}

// 新建项目
async function createProject() {
  error.value = null
  message.value = null
  try {
    const path = await invoke<string | null>('open_folder_dialog', { title: '选择新项目目录' })
    if (!path) return
    projectDir.value = path
    project.value = await invoke<ProjectInfo>('project_new', {
      dir: path,
      name: newName.value,
      chip: newChip.value,
    })
    message.value = `已创建项目: ${newName.value}`
    showNewDialog.value = false
    isDirty.value = false
    await saveRecentProject(path, newName.value, newChip.value)
  } catch (e) {
    error.value = String(e)
  }
}

// 保存项目
async function saveProject() {
  if (!project.value || !projectDir.value) return
  error.value = null
  message.value = null
  isSaving.value = true
  try {
    project.value = await invoke<ProjectInfo>('project_save', {
      dir: projectDir.value,
      info: project.value,
    })
    message.value = '项目配置已保存'
    isDirty.value = false
    await saveRecentProject(projectDir.value, project.value.name, project.value.chip)
  } catch (e) {
    error.value = String(e)
  } finally {
    isSaving.value = false
  }
}

// 导入 LuaTools INI
async function importIni() {
  error.value = null
  message.value = null
  try {
    const iniPath = await invoke<string | null>('open_file_dialog', {
      title: '选择 LuaTools INI 文件',
      filterName: 'INI 配置',
      extensions: ['ini'],
    })
    if (!iniPath) return

    const outDir = await invoke<string | null>('open_folder_dialog', { title: '选择导入目标目录' })
    if (!outDir) return

    projectDir.value = outDir
    project.value = await invoke<ProjectInfo>('project_import', {
      iniPath,
      outputDir: outDir,
    })
    message.value = `已导入 LuaTools 项目: ${project.value.name}`
    isDirty.value = false
    await saveRecentProject(outDir, project.value.name, project.value.chip)
  } catch (e) {
    error.value = String(e)
  }
}

// 选择 SOC 文件
async function browseSoc() {
  if (!project.value) return
  try {
    const path = await invoke<string | null>('open_file_dialog', {
      title: '选择固件 (.soc)',
      filterName: 'LuatOS 固件',
      extensions: ['soc'],
    })
    if (path) {
      project.value.soc_file = path
      markDirty()
    }
  } catch (e) {
    error.value = String(e)
  }
}

// 添加脚本目录
function addScriptDir() {
  if (!project.value || !newScriptDir.value.trim()) return
  if (!project.value.script_dirs.includes(newScriptDir.value.trim())) {
    project.value.script_dirs.push(newScriptDir.value.trim())
    markDirty()
  }
  newScriptDir.value = ''
}

// 删除脚本目录
function removeScriptDir(index: number) {
  if (!project.value) return
  project.value.script_dirs.splice(index, 1)
  markDirty()
}

onMounted(async () => {
  await loadRecentProjects()
})
</script>

<template>
  <div class="flex gap-4 h-full">

    <!-- 左侧：最近项目列表 -->
    <aside v-if="recentProjects.length > 0" class="w-52 shrink-0 bg-gray-900 border border-gray-800 rounded-lg p-3 space-y-2 overflow-y-auto">
      <h2 class="text-xs font-semibold text-gray-500 uppercase tracking-wider mb-2">最近项目</h2>
      <button
        v-for="r in recentProjects"
        :key="r.path"
        @click="switchToRecent(r)"
        :class="[
          'w-full text-left px-3 py-2 rounded-md text-sm transition-colors',
          projectDir === r.path
            ? 'bg-cyan-700 text-white'
            : 'text-gray-400 hover:bg-gray-800 hover:text-gray-200',
        ]"
      >
        <div class="font-medium truncate">{{ r.name }}</div>
        <div class="text-xs opacity-60 truncate">{{ r.chip }}</div>
      </button>
    </aside>

    <!-- 右侧：主内容 -->
    <div class="flex-1 space-y-4 overflow-y-auto">
      <h1 class="text-xl font-bold text-gray-100">📁 项目管理</h1>

      <!-- 操作按钮 -->
      <div class="flex gap-2 flex-wrap">
        <button @click="browseProject" class="px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200 transition-colors">
          📂 打开项目
        </button>
        <button @click="showNewDialog = !showNewDialog" class="px-4 py-2 bg-cyan-700 hover:bg-cyan-600 rounded text-sm text-white transition-colors">
          ✨ 新建项目
        </button>
        <button @click="importIni" class="px-4 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200 transition-colors">
          📥 导入 LuaTools INI
        </button>
        <button v-if="project && isDirty" @click="saveProject" :disabled="isSaving"
          class="px-4 py-2 bg-green-700 hover:bg-green-600 disabled:opacity-50 rounded text-sm text-white font-semibold transition-colors">
          {{ isSaving ? '保存中...' : '💾 保存' }}
        </button>
      </div>

      <!-- 新建项目对话 -->
      <div v-if="showNewDialog" class="bg-gray-900 border border-cyan-800 rounded-lg p-4 space-y-3">
        <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">新建项目</h2>
        <div class="grid grid-cols-2 gap-3">
          <div>
            <label class="block text-xs text-gray-500 mb-1">项目名称</label>
            <input v-model="newName" class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 focus:outline-none focus:border-cyan-600" />
          </div>
          <div>
            <label class="block text-xs text-gray-500 mb-1">目标芯片</label>
            <select v-model="newChip" class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 focus:outline-none">
              <option v-for="c in chips" :key="c" :value="c">{{ c }}</option>
            </select>
          </div>
        </div>
        <div class="flex gap-2 justify-end">
          <button @click="showNewDialog = false" class="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 rounded text-sm text-gray-400 transition-colors">取消</button>
          <button @click="createProject" class="px-4 py-1.5 bg-cyan-600 hover:bg-cyan-500 rounded text-sm font-semibold text-white transition-colors">创建</button>
        </div>
      </div>

      <!-- 消息/错误 -->
      <p v-if="message" class="text-sm text-green-400">✅ {{ message }}</p>
      <p v-if="error" class="text-sm text-red-400">❌ {{ error }}</p>

      <!-- 项目信息（可编辑） -->
      <div v-if="project" class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-4">
        <div class="flex items-center justify-between">
          <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">
            项目配置
            <span v-if="isDirty" class="ml-2 text-yellow-400">● 未保存</span>
          </h2>
          <span class="text-xs text-gray-600 truncate max-w-xs">{{ projectDir }}</span>
        </div>

        <!-- 基本信息 -->
        <div class="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
          <div>
            <label class="block text-xs text-gray-500 mb-1">名称</label>
            <input v-model="project.name" @input="markDirty"
              class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-cyan-600" />
          </div>
          <div>
            <label class="block text-xs text-gray-500 mb-1">芯片</label>
            <select v-model="project.chip" @change="markDirty"
              class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none">
              <option v-for="c in chips" :key="c" :value="c">{{ c }}</option>
            </select>
          </div>
          <div>
            <label class="block text-xs text-gray-500 mb-1">版本</label>
            <input v-model="project.version" @input="markDirty"
              class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-cyan-600" />
          </div>
          <div>
            <label class="block text-xs text-gray-500 mb-1">描述</label>
            <input v-model="project.description" @input="markDirty" placeholder="可选"
              class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-cyan-600" />
          </div>
        </div>

        <!-- 构建配置 -->
        <div class="border-t border-gray-800 pt-3">
          <h3 class="text-xs font-semibold text-gray-500 mb-3">构建设置</h3>
          <div class="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
            <div>
              <label class="block text-xs text-gray-500 mb-1">输出目录</label>
              <input v-model="project.output_dir" @input="markDirty"
                class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-cyan-600" />
            </div>
            <div>
              <label class="block text-xs text-gray-500 mb-1">Lua 位宽</label>
              <select v-model.number="project.bitw" @change="markDirty"
                class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none">
                <option :value="32">32 bit</option>
                <option :value="64">64 bit</option>
              </select>
            </div>
          </div>
          <div class="flex gap-6 mt-3">
            <label class="flex items-center gap-1.5 text-sm text-gray-400 cursor-pointer">
              <input type="checkbox" v-model="project.use_luac" @change="markDirty" class="accent-cyan-500" />
              编译 luac
            </label>
            <label class="flex items-center gap-1.5 text-sm text-gray-400 cursor-pointer">
              <input type="checkbox" v-model="project.luac_debug" @change="markDirty" class="accent-cyan-500" />
              保留 Debug 信息
            </label>
            <label class="flex items-center gap-1.5 text-sm text-gray-400 cursor-pointer">
              <input type="checkbox" v-model="project.ignore_deps" @change="markDirty" class="accent-cyan-500" />
              忽略依赖检查
            </label>
          </div>
        </div>

        <!-- 脚本目录 -->
        <div class="border-t border-gray-800 pt-3">
          <h3 class="text-xs font-semibold text-gray-500 mb-2">脚本目录</h3>
          <div class="flex flex-wrap gap-2 mb-2">
            <span v-for="(d, i) in project.script_dirs" :key="d"
              class="bg-gray-800 rounded px-2 py-1 text-xs text-gray-300 flex items-center gap-1.5 group">
              {{ d }}
              <button @click="removeScriptDir(i)" class="text-gray-600 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity">✕</button>
            </span>
            <span v-if="project.script_dirs.length === 0" class="text-xs text-gray-600 italic">无</span>
          </div>
          <div class="flex gap-2">
            <input v-model="newScriptDir" @keyup.enter="addScriptDir" placeholder="添加脚本目录 (如 lua/)"
              class="flex-1 bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-xs text-gray-300 placeholder-gray-600 focus:outline-none focus:border-cyan-600" />
            <button @click="addScriptDir" :disabled="!newScriptDir.trim()"
              class="px-3 py-1.5 bg-gray-700 hover:bg-gray-600 disabled:opacity-40 rounded text-xs text-gray-300 transition-colors">+</button>
          </div>
        </div>

        <!-- 刷机配置 -->
        <div class="border-t border-gray-800 pt-3">
          <h3 class="text-xs font-semibold text-gray-500 mb-3">刷机设置</h3>
          <div class="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
            <div class="col-span-2">
              <label class="block text-xs text-gray-500 mb-1">SOC 固件文件</label>
              <div class="flex gap-2">
                <input :value="project.soc_file || ''" readonly placeholder="选择 .soc 文件..."
                  class="flex-1 bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-300 placeholder-gray-600 focus:outline-none cursor-pointer" @click="browseSoc" />
                <button @click="browseSoc" class="px-3 py-1.5 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-300 transition-colors">浏览</button>
              </div>
            </div>
            <div>
              <label class="block text-xs text-gray-500 mb-1">串口</label>
              <input v-model="project.port" @input="markDirty" placeholder="自动 (继承全局)"
                class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-cyan-600" />
            </div>
            <div>
              <label class="block text-xs text-gray-500 mb-1">波特率</label>
              <input v-model.number="project.baud" @input="markDirty" placeholder="自动"
                class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-cyan-600" />
            </div>
          </div>
        </div>

        <!-- 保存按钮（底部） -->
        <div v-if="isDirty" class="flex justify-end pt-2">
          <button @click="saveProject" :disabled="isSaving"
            class="px-6 py-2 bg-green-700 hover:bg-green-600 disabled:opacity-50 rounded text-sm font-semibold text-white transition-colors">
            {{ isSaving ? '保存中...' : '💾 保存项目配置' }}
          </button>
        </div>
      </div>

      <!-- 无项目时提示 -->
      <div v-if="!project && !showNewDialog" class="bg-gray-900 border border-gray-800 rounded-lg p-8 text-center space-y-3">
        <p class="text-gray-500 text-sm">打开已有项目、新建项目或导入 LuaTools INI 文件开始</p>
      </div>
    </div>
  </div>
</template>
