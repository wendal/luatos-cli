<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn, type Event } from '@tauri-apps/api/event'
import { useSerialStore } from '../stores/serial'

interface LogLine {
  text: string
  timestamp_ms: number
}

interface LogStatus {
  connected: boolean
  message: string
}

const serial = useSerialStore()

// 日志模式
type LogMode = 'text' | 'binary'
const logMode = ref<LogMode>('text')
const probe = ref(false)

// 状态
const isRunning = ref(false)
const isConnected = ref(false)
const statusMsg = ref('')
const errorMsg = ref<string | null>(null)

// 日志数据
const lines = ref<{ text: string; ts: string }[]>([])
const filter = ref('')
const autoScroll = ref(true)
const maxLines = 5000

const logContainer = ref<HTMLElement | null>(null)

let unlistenLine: UnlistenFn | null = null
let unlistenStatus: UnlistenFn | null = null

// 格式化时间戳
function formatTs(ms: number): string {
  const total = Math.floor(ms / 1000)
  const s = total % 60
  const m = Math.floor(total / 60) % 60
  const h = Math.floor(total / 3600)
  const frac = String(ms % 1000).padStart(3, '0')
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}.${frac}`
}

// 行颜色
function lineClass(text: string): string {
  if (text.startsWith('E/') || text.includes('[ERROR]') || text.includes('ERROR')) return 'text-red-400'
  if (text.startsWith('W/') || text.includes('[WARN]') || text.includes('WARN')) return 'text-yellow-400'
  if (text.startsWith('I/') || text.includes('[INFO]') || text.includes('INFO')) return 'text-cyan-300'
  if (text.startsWith('D/') || text.includes('[DEBUG]')) return 'text-gray-500'
  return 'text-gray-300'
}

// 过滤后的行
const filteredLines = computed(() => {
  if (!filter.value) return lines.value
  const f = filter.value.toLowerCase()
  return lines.value.filter(l => l.text.toLowerCase().includes(f))
})

// 自动滚动
async function scrollToBottom() {
  if (!autoScroll.value || !logContainer.value) return
  await nextTick()
  logContainer.value.scrollTop = logContainer.value.scrollHeight
}

// 启动日志
async function startLog() {
  if (!serial.selectedPort) return
  isRunning.value = true
  errorMsg.value = null
  lines.value = []

  try {
    if (logMode.value === 'text') {
      await invoke('log_start', {
        port: serial.selectedPort,
        baudRate: serial.baudRate,
      })
    } else {
      await invoke('log_start_binary', {
        port: serial.selectedPort,
        baudRate: serial.baudRate,
        probe: probe.value,
      })
    }
  } catch (e) {
    isRunning.value = false
    errorMsg.value = String(e)
    statusMsg.value = ''
  }
}

// 停止日志
async function stopLog() {
  try {
    await invoke('log_stop')
  } catch (_) { /* 忽略停止错误 */ }
  isRunning.value = false
  isConnected.value = false
}

// 清空
function clearLog() {
  lines.value = []
  errorMsg.value = null
}

// 事件监听
async function setupListeners() {
  unlistenLine = await listen<LogLine>('log:line', (evt: Event<LogLine>) => {
    const { text, timestamp_ms } = evt.payload
    lines.value.push({ text, ts: formatTs(timestamp_ms) })
    if (lines.value.length > maxLines) {
      lines.value.splice(0, lines.value.length - maxLines)
    }
    scrollToBottom()
  })

  unlistenStatus = await listen<LogStatus>('log:status', (evt: Event<LogStatus>) => {
    const { connected, message } = evt.payload
    isConnected.value = connected
    statusMsg.value = message
    if (!connected) {
      isRunning.value = false
      // 如果是错误断开，显示错误
      if (message.includes('错误') || message.includes('失败')) {
        errorMsg.value = message
      }
    }
  })
}

// 加载设置中的波特率
async function loadBaudFromSettings() {
  try {
    const settings = await invoke<any>('settings_load')
    if (settings?.default_baud_rate) {
      serial.baudRate = settings.default_baud_rate
    }
  } catch (_) { /* 使用默认值 */ }
}

onMounted(async () => {
  await setupListeners()
  await loadBaudFromSettings()
})

onUnmounted(() => {
  unlistenLine?.()
  unlistenStatus?.()
  invoke('log_stop').catch(() => {})
})
</script>

<template>
  <div class="flex flex-col h-full space-y-3">

    <!-- 标题栏 -->
    <div class="flex items-center justify-between">
      <h1 class="text-xl font-bold text-gray-100">📋 日志查看器</h1>
      <div class="flex items-center gap-1.5">
        <span class="w-2 h-2 rounded-full" :class="isConnected ? 'bg-green-400 animate-pulse' : 'bg-gray-600'" />
        <span class="text-xs text-gray-500">{{ statusMsg || (isConnected ? '已连接' : '未连接') }}</span>
      </div>
    </div>

    <!-- 错误提示 -->
    <div v-if="errorMsg" class="bg-red-900/50 border border-red-700 rounded-lg px-4 py-3 flex items-center gap-3">
      <span class="text-red-400 text-lg">⚠</span>
      <div class="flex-1">
        <p class="text-sm text-red-300 font-semibold">连接失败</p>
        <p class="text-xs text-red-400 mt-0.5">{{ errorMsg }}</p>
      </div>
      <button @click="errorMsg = null" class="text-red-500 hover:text-red-300 text-sm px-2">✕</button>
    </div>

    <!-- 控制栏 -->
    <div class="bg-gray-900 border border-gray-800 rounded-lg p-3 flex flex-wrap gap-2 items-center">
      <!-- 模式 -->
      <select
        v-model="logMode"
        :disabled="isRunning"
        class="bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-gray-300 focus:outline-none"
      >
        <option value="text">文本模式</option>
        <option value="binary">二进制模式</option>
      </select>

      <!-- 探测 (二进制模式) -->
      <label v-if="logMode === 'binary'" class="flex items-center gap-1 text-xs text-gray-400 cursor-pointer">
        <input type="checkbox" v-model="probe" :disabled="isRunning" class="accent-cyan-500" />
        探测 (probe)
      </label>

      <!-- 开始/停止 -->
      <button v-if="!isRunning" @click="startLog" :disabled="!serial.selectedPort"
        class="px-4 py-1.5 bg-cyan-600 hover:bg-cyan-500 disabled:opacity-40 rounded text-sm font-semibold text-white transition-colors">
        ▶ 开始
      </button>
      <button v-else @click="stopLog"
        class="px-4 py-1.5 bg-orange-600 hover:bg-orange-500 rounded text-sm font-semibold text-white transition-colors">
        ■ 停止
      </button>

      <div class="ml-auto flex items-center gap-2">
        <label class="flex items-center gap-1 text-xs text-gray-500 cursor-pointer">
          <input type="checkbox" v-model="autoScroll" class="accent-cyan-500" />
          自动滚动
        </label>
        <span class="text-xs text-gray-600">{{ lines.length }} 行</span>
        <button @click="clearLog" class="px-2 py-1.5 bg-gray-800 hover:bg-gray-700 rounded text-xs text-gray-400 transition-colors">
          清空
        </button>
      </div>
    </div>

    <!-- 过滤 -->
    <input
      v-model="filter"
      type="text"
      placeholder="过滤关键字..."
      class="bg-gray-900 border border-gray-800 rounded px-3 py-2 text-sm text-gray-300 placeholder-gray-600 focus:outline-none focus:border-cyan-600"
    />

    <!-- 日志区域 -->
    <div
      ref="logContainer"
      class="flex-1 bg-gray-950 border border-gray-800 rounded-lg p-3 overflow-y-auto font-mono text-xs leading-5 min-h-0"
      style="min-height: 200px"
    >
      <div
        v-for="(entry, i) in filteredLines"
        :key="i"
        :class="lineClass(entry.text)"
        class="flex gap-2 hover:bg-gray-900/50"
      >
        <span class="text-gray-700 select-none shrink-0">{{ entry.ts }}</span>
        <span class="break-all">{{ entry.text }}</span>
      </div>
      <div v-if="lines.length === 0 && !errorMsg" class="text-gray-700 italic">
        {{ isRunning ? '等待设备输出...' : '点击"开始"连接串口查看日志' }}
      </div>
    </div>
  </div>
</template>
