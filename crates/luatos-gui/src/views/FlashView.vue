<script setup lang="ts">
import { ref, onUnmounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn, type Event } from '@tauri-apps/api/event'
import { useSerialStore } from '../stores/serial'

interface SocInfoResult {
  chip_type: string
  version: string
  files: string[]
}

interface FlashEvent {
  stage: string
  percent: number
  message: string
  done: boolean
  error: boolean
}

const emit = defineEmits<{
  (e: 'switch-to', view: string): void
}>()

const serial = useSerialStore()

// 固件
const socPath = ref('')
const socInfo = ref<SocInfoResult | null>(null)
const socError = ref<string | null>(null)

// 操作类型
type FlashOp = 'full' | 'script'
const flashOp = ref<FlashOp>('full')
const scriptFolder = ref('')

// 状态
const isFlashing = ref(false)
const flashDone = ref(false)
const flashError = ref(false)
const flashStage = ref('')
const flashPercent = ref(0)
const flashLog = ref<string[]>([])

let unlistenFlash: UnlistenFn | null = null

// 选择 SOC 文件
async function browseSoc() {
  try {
    const path = await invoke<string | null>('open_file_dialog', {
      title: '选择固件 (.soc)',
      filterName: 'LuatOS 固件',
      extensions: ['soc'],
    })
    if (path) {
      socPath.value = path
      await loadSocInfo()
    }
  } catch (e) {
    socError.value = String(e)
  }
}

// 加载 SOC 信息
async function loadSocInfo() {
  socError.value = null
  socInfo.value = null
  try {
    socInfo.value = await invoke<SocInfoResult>('soc_info', { socPath: socPath.value })
  } catch (e) {
    socError.value = String(e)
  }
}

// 选择脚本目录
async function browseScriptFolder() {
  try {
    const path = await invoke<string | null>('open_folder_dialog', {
      title: '选择脚本目录',
    })
    if (path) {
      scriptFolder.value = path
    }
  } catch (e) {
    socError.value = String(e)
  }
}

// 开始刷机
async function startFlash() {
  if (!socPath.value || !serial.selectedPort) return

  isFlashing.value = true
  flashDone.value = false
  flashError.value = false
  flashStage.value = '准备中...'
  flashPercent.value = 0
  flashLog.value = []

  // 监听进度事件
  unlistenFlash = await listen<FlashEvent>('flash:progress', (evt: Event<FlashEvent>) => {
    const { stage, percent, message, done, error } = evt.payload
    if (message) flashLog.value.push(message)
    if (percent >= 0) flashPercent.value = Math.round(percent)
    if (stage) flashStage.value = stage
    if (done) {
      isFlashing.value = false
      flashDone.value = true
      flashError.value = error
    }
  })

  // 构造参数
  const scriptFolders = flashOp.value === 'script' && scriptFolder.value
    ? [scriptFolder.value]
    : null

  try {
    await invoke('flash_run', {
      socPath: socPath.value,
      port: serial.selectedPort,
      baudRate: null,
      scriptFolders,
    })
  } catch (e) {
    isFlashing.value = false
    flashDone.value = true
    flashError.value = true
    flashLog.value.push(`错误: ${e}`)
  }
}

// 取消刷机
async function cancelFlash() {
  await invoke('flash_cancel')
  isFlashing.value = false
  flashLog.value.push('[取消] 刷机已取消')
}

// 跳转日志
function goToLog() {
  emit('switch-to', 'log')
}

onUnmounted(() => {
  unlistenFlash?.()
})
</script>

<template>
  <div class="space-y-4">
    <h1 class="text-xl font-bold text-gray-100">⚡ 刷机</h1>

    <!-- 固件选择 -->
    <div class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-3">
      <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">固件 (.soc)</h2>
      <div class="flex gap-2">
        <input
          v-model="socPath"
          type="text"
          readonly
          placeholder="未选择固件..."
          @click="browseSoc"
          class="flex-1 bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 placeholder-gray-600 cursor-pointer focus:outline-none focus:border-cyan-600"
        />
        <button @click="browseSoc" class="px-3 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200 transition-colors">浏览</button>
      </div>
      <p v-if="socError" class="text-xs text-red-400">{{ socError }}</p>
      <div v-if="socInfo" class="flex flex-wrap gap-3 text-xs">
        <span class="bg-gray-800 rounded px-2 py-1 text-cyan-300">芯片: {{ socInfo.chip_type }}</span>
        <span v-if="socInfo.version" class="bg-gray-800 rounded px-2 py-1 text-green-300">版本: {{ socInfo.version }}</span>
        <span class="bg-gray-800 rounded px-2 py-1 text-gray-400">{{ socInfo.files.length }} 个文件</span>
      </div>
    </div>

    <!-- 操作类型 -->
    <div class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-3">
      <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">操作类型</h2>
      <div class="flex gap-4">
        <label class="flex items-center gap-2 text-sm cursor-pointer" :class="flashOp === 'full' ? 'text-cyan-300' : 'text-gray-400'">
          <input type="radio" v-model="flashOp" value="full" :disabled="isFlashing" class="accent-cyan-500" />
          全量刷机
        </label>
        <label class="flex items-center gap-2 text-sm cursor-pointer" :class="flashOp === 'script' ? 'text-cyan-300' : 'text-gray-400'">
          <input type="radio" v-model="flashOp" value="script" :disabled="isFlashing" class="accent-cyan-500" />
          刷脚本区
        </label>
      </div>

      <!-- 脚本目录选择 (仅刷脚本区时显示) -->
      <div v-if="flashOp === 'script'" class="flex gap-2 mt-2">
        <input
          v-model="scriptFolder"
          type="text"
          readonly
          placeholder="选择脚本目录..."
          @click="browseScriptFolder"
          class="flex-1 bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm text-gray-300 placeholder-gray-600 cursor-pointer focus:outline-none focus:border-cyan-600"
        />
        <button @click="browseScriptFolder" class="px-3 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-gray-200 transition-colors">浏览</button>
      </div>
    </div>

    <!-- 操作按钮 -->
    <div class="flex gap-3 items-center">
      <button
        v-if="!isFlashing"
        @click="startFlash"
        :disabled="!socPath || !serial.selectedPort || (flashOp === 'script' && !scriptFolder)"
        class="flex-1 py-3 bg-cyan-600 hover:bg-cyan-500 active:bg-cyan-700 disabled:opacity-40 disabled:cursor-not-allowed rounded-lg text-base font-bold text-white transition-colors"
      >
        ⚡ {{ flashOp === 'full' ? '开始全量刷机' : '刷入脚本' }}
      </button>
      <button
        v-else
        @click="cancelFlash"
        class="flex-1 py-3 bg-red-700 hover:bg-red-600 rounded-lg text-base font-bold text-white transition-colors"
      >
        ✕ 取消刷机
      </button>
    </div>

    <!-- 进度 -->
    <div class="bg-gray-900 border border-gray-800 rounded-lg p-4 space-y-3">
      <div class="flex items-center justify-between">
        <h2 class="text-xs font-semibold text-cyan-400 uppercase tracking-wider">进度</h2>
        <span class="text-xs font-mono" :class="flashError ? 'text-red-400' : flashDone ? 'text-green-400' : 'text-gray-500'">
          {{ flashDone ? (flashError ? '❌ 失败' : '✅ 完成') : (isFlashing ? flashStage : '等待开始') }}
        </span>
      </div>

      <div class="w-full bg-gray-800 rounded-full h-2.5">
        <div
          class="h-2.5 rounded-full transition-all duration-300"
          :class="flashError ? 'bg-red-500' : flashDone ? 'bg-green-500' : 'bg-cyan-500'"
          :style="{ width: flashPercent + '%' }"
        />
      </div>
      <p class="text-xs text-gray-500 font-mono">
        {{ isFlashing ? `${flashStage} — ${flashPercent}%` : flashDone ? (flashError ? '刷机失败' : '刷机成功') : '等待开始...' }}
      </p>

      <!-- 刷机成功后跳转日志 -->
      <div v-if="flashDone && !flashError" class="flex justify-end">
        <button @click="goToLog" class="px-4 py-1.5 bg-green-700 hover:bg-green-600 rounded text-sm font-semibold text-white transition-colors">
          📋 查看日志 →
        </button>
      </div>

      <!-- 刷机日志 -->
      <div v-if="flashLog.length > 0"
        class="bg-gray-950 rounded p-3 font-mono text-xs leading-5 max-h-48 overflow-y-auto space-y-0.5">
        <div v-for="(line, i) in flashLog.slice(-30)" :key="i"
          :class="line.includes('错误') || line.includes('ERROR') || line.includes('Failed') ? 'text-red-400' :
                  line.includes('WARN') || line.includes('Warning') ? 'text-yellow-400' :
                  line.includes('OK') || line.includes('完成') || line.includes('Finished') ? 'text-green-400' :
                  'text-gray-400'">
          {{ line }}
        </div>
      </div>
    </div>
  </div>
</template>
