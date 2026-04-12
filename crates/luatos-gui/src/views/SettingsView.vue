<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'

interface AppSettings {
  default_baud_rate: number
  default_soc_path: string | null
  log_max_lines: number
  auto_switch_to_log: boolean
  log_save_dir: string | null
}

const settings = ref<AppSettings>({
  default_baud_rate: 921600,
  default_soc_path: null,
  log_max_lines: 5000,
  auto_switch_to_log: true,
  log_save_dir: null,
})

const saved = ref(false)
const error = ref<string | null>(null)

const baudRates = [9600, 115200, 460800, 921600, 1500000, 2000000]

// 加载设置
async function loadSettings() {
  try {
    settings.value = await invoke<AppSettings>('settings_load')
  } catch (e) {
    error.value = String(e)
  }
}

// 保存设置
async function saveSettings() {
  error.value = null
  saved.value = false
  try {
    await invoke('settings_save', { settings: settings.value })
    saved.value = true
    setTimeout(() => { saved.value = false }, 2000)
  } catch (e) {
    error.value = String(e)
  }
}

// 选择日志目录
async function browseLogDir() {
  try {
    const path = await invoke<string | null>('open_folder_dialog', { title: '选择日志保存目录' })
    if (path) {
      settings.value.log_save_dir = path
    }
  } catch (e) {
    error.value = String(e)
  }
}

onMounted(loadSettings)
</script>

<template>
  <div class="space-y-4">
    <h1 class="text-xl font-bold text-gray-100">⚙️ 设置</h1>

    <div class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-4">
      <!-- 串口 -->
      <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">串口</h2>
      <div class="grid grid-cols-2 gap-4">
        <div>
          <label class="block text-xs text-gray-500 mb-1">默认波特率</label>
          <select v-model.number="settings.default_baud_rate"
            class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 focus:outline-none">
            <option v-for="b in baudRates" :key="b" :value="b">{{ b }}</option>
          </select>
        </div>
        <div>
          <label class="block text-xs text-gray-500 mb-1">默认固件路径</label>
          <input v-model="settings.default_soc_path" placeholder="(空)"
            class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 placeholder-gray-600 focus:outline-none focus:border-cyan-600" />
        </div>
      </div>

      <!-- 日志 -->
      <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider mt-4">日志</h2>
      <div class="grid grid-cols-2 gap-4">
        <div>
          <label class="block text-xs text-gray-500 mb-1">最大日志行数</label>
          <input v-model.number="settings.log_max_lines" type="number" min="500" max="50000" step="500"
            class="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 focus:outline-none focus:border-cyan-600" />
        </div>
        <div>
          <label class="block text-xs text-gray-500 mb-1">日志保存目录</label>
          <div class="flex gap-2">
            <input v-model="settings.log_save_dir" readonly placeholder="默认" @click="browseLogDir"
              class="flex-1 bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 placeholder-gray-600 cursor-pointer focus:outline-none" />
            <button @click="browseLogDir" class="px-2 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200">浏览</button>
          </div>
        </div>
      </div>

      <!-- 行为 -->
      <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider mt-4">行为</h2>
      <label class="flex items-center gap-2 text-sm text-gray-400 cursor-pointer">
        <input type="checkbox" v-model="settings.auto_switch_to_log" class="accent-cyan-500" />
        刷机完成后自动跳转日志
      </label>
    </div>

    <!-- 保存按钮 -->
    <div class="flex gap-3 items-center">
      <button @click="saveSettings"
        class="px-6 py-2.5 bg-cyan-600 hover:bg-cyan-500 rounded-lg text-sm font-bold text-white transition-colors">
        💾 保存设置
      </button>
      <span v-if="saved" class="text-sm text-green-400">✅ 已保存</span>
      <span v-if="error" class="text-sm text-red-400">❌ {{ error }}</span>
    </div>
  </div>
</template>
