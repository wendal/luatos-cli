<script setup lang="ts">
import { onMounted } from 'vue'
import { useSerialStore } from '../stores/serial'

const serial = useSerialStore()

onMounted(() => {
  serial.refreshPorts()
})
</script>

<template>
  <div class="flex items-center gap-2 bg-gray-900 border border-gray-800 rounded-lg px-3 py-2">
    <!-- 串口选择 -->
    <label class="text-xs text-gray-500 shrink-0">串口</label>
    <select
      v-model="serial.selectedPort"
      :disabled="serial.isLoading"
      class="bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-gray-300 focus:outline-none focus:border-cyan-600 min-w-[200px]"
    >
      <option value="">— 请选择串口 —</option>
      <option v-for="p in serial.ports" :key="p.port_name" :value="p.port_name">
        {{ serial.portLabel(p) }}
      </option>
    </select>

    <!-- 波特率 -->
    <label class="text-xs text-gray-500 shrink-0 ml-2">波特率</label>
    <select
      v-model.number="serial.baudRate"
      class="bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-gray-300 focus:outline-none focus:border-cyan-600 w-28"
    >
      <option :value="2000000">2000000</option>
      <option :value="921600">921600</option>
      <option :value="115200">115200</option>
      <option :value="9600">9600</option>
    </select>

    <!-- 刷新按钮 -->
    <button
      @click="serial.refreshPorts()"
      :disabled="serial.isLoading"
      class="px-2 py-1.5 bg-gray-700 hover:bg-gray-600 disabled:opacity-40 rounded text-xs text-gray-300 transition-colors shrink-0"
    >
      {{ serial.isLoading ? '...' : '🔄 刷新' }}
    </button>

    <!-- 连接状态 -->
    <div class="flex items-center gap-1.5 ml-auto shrink-0">
      <span
        class="inline-block w-2 h-2 rounded-full"
        :class="serial.selectedPort ? 'bg-green-400' : 'bg-gray-600'"
      />
      <span class="text-xs" :class="serial.selectedPort ? 'text-green-400' : 'text-gray-600'">
        {{ serial.selectedPort || '未选择' }}
      </span>
    </div>

    <!-- 错误提示 -->
    <span v-if="serial.error" class="text-xs text-yellow-400 ml-2">{{ serial.error }}</span>
  </div>
</template>
