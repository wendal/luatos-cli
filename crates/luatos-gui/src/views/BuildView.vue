<script setup lang="ts">
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

interface BuildResult {
  output_path: string
  file_count: number
  size_bytes: number
}

// 构建参数
const srcDirs = ref<string[]>([])
const outputPath = ref('')
const useLuac = ref(true)
const bitw = ref(32)
const bkcrc = ref(false)

// 状态
const isBuilding = ref(false)
const result = ref<BuildResult | null>(null)
const error = ref<string | null>(null)

// 添加源目录
async function addSrcDir() {
  try {
    const path = await invoke<string | null>('open_folder_dialog', { title: '选择 Lua 脚本目录' })
    if (path && !srcDirs.value.includes(path)) {
      srcDirs.value.push(path)
    }
  } catch (e) {
    error.value = String(e)
  }
}

// 移除源目录
function removeSrcDir(index: number) {
  srcDirs.value.splice(index, 1)
}

// 选择输出路径
async function browseOutput() {
  try {
    const path = await invoke<string | null>('open_folder_dialog', { title: '选择输出目录' })
    if (path) {
      outputPath.value = path
    }
  } catch (e) {
    error.value = String(e)
  }
}

// 构建文件系统镜像
async function buildFs() {
  if (srcDirs.value.length === 0 || !outputPath.value) return

  isBuilding.value = true
  error.value = null
  result.value = null

  try {
    const outFile = outputPath.value.replace(/[\\/]$/, '') + '\\script.bin'
    result.value = await invoke<BuildResult>('build_filesystem', {
      srcDirs: srcDirs.value,
      outputPath: outFile,
      useLuac: useLuac.value,
      bitw: bitw.value,
      bkcrc: bkcrc.value,
    })
  } catch (e) {
    error.value = String(e)
  } finally {
    isBuilding.value = false
  }
}
</script>

<template>
  <div class="space-y-4">
    <h1 class="text-xl font-bold text-gray-100">🔨 构建</h1>

    <!-- 源目录 -->
    <div class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-3">
      <div class="flex items-center justify-between">
        <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">Lua 脚本目录</h2>
        <button @click="addSrcDir" class="px-3 py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs text-gray-300 transition-colors">+ 添加目录</button>
      </div>
      <div v-if="srcDirs.length === 0" class="text-xs text-gray-600 italic">未添加脚本目录</div>
      <div v-for="(dir, i) in srcDirs" :key="i" class="flex items-center gap-2 bg-gray-800 rounded px-3 py-2 text-sm text-gray-300">
        <span class="flex-1 truncate">{{ dir }}</span>
        <button @click="removeSrcDir(i)" class="text-red-500 hover:text-red-400 text-xs">✕</button>
      </div>
    </div>

    <!-- 构建选项 -->
    <div class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-3">
      <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">构建选项</h2>
      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="block text-xs text-gray-500 mb-1">输出目录</label>
          <div class="flex gap-2">
            <input v-model="outputPath" readonly placeholder="选择输出目录..." @click="browseOutput"
              class="flex-1 bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 placeholder-gray-600 cursor-pointer focus:outline-none" />
            <button @click="browseOutput" class="px-2 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200">浏览</button>
          </div>
        </div>
        <div>
          <label class="block text-xs text-gray-500 mb-1">Lua 位宽</label>
          <select v-model.number="bitw" class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 focus:outline-none">
            <option :value="32">32 bit</option>
            <option :value="64">64 bit</option>
          </select>
        </div>
      </div>

      <div class="flex gap-6">
        <label class="flex items-center gap-2 text-sm text-gray-400 cursor-pointer">
          <input type="checkbox" v-model="useLuac" class="accent-cyan-500" />
          Luac 编译
        </label>
        <label class="flex items-center gap-2 text-sm text-gray-400 cursor-pointer">
          <input type="checkbox" v-model="bkcrc" class="accent-cyan-500" />
          BK CRC16
        </label>
      </div>
    </div>

    <!-- 构建按钮 -->
    <button
      @click="buildFs"
      :disabled="isBuilding || srcDirs.length === 0 || !outputPath"
      class="w-full py-3 bg-cyan-600 hover:bg-cyan-500 active:bg-cyan-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg text-base font-bold text-white transition-colors"
    >
      {{ isBuilding ? '⏳ 构建中...' : '🔨 构建文件系统镜像' }}
    </button>

    <!-- 结果 -->
    <div v-if="result" class="bg-gray-900 border border-green-900 rounded-lg p-4 space-y-2">
      <h2 class="text-xs font-semibold text-green-400 uppercase tracking-wider">✅ 构建完成</h2>
      <div class="text-sm text-gray-300 space-y-1">
        <p>输出: <span class="text-gray-200 font-mono">{{ result.output_path }}</span></p>
        <p v-if="result.size_bytes > 0">大小: <span class="text-gray-200">{{ (result.size_bytes / 1024).toFixed(1) }} KB</span></p>
      </div>
    </div>

    <p v-if="error" class="text-sm text-red-400">❌ {{ error }}</p>
  </div>
</template>
