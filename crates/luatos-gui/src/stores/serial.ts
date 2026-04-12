import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export interface PortInfo {
  port_name: string
  vid: string | null
  pid: string | null
  manufacturer: string | null
  product: string | null
  serial_number: string | null
}

export const useSerialStore = defineStore('serial', () => {
  const ports = ref<PortInfo[]>([])
  const selectedPort = ref('')
  const baudRate = ref(115200)
  const isLoading = ref(false)
  const error = ref<string | null>(null)

  async function refreshPorts() {
    isLoading.value = true
    error.value = null
    try {
      const result = await invoke<PortInfo[]>('serial_list')
      ports.value = result
      // 如果当前选中的串口不在列表中，自动选第一个
      if (!result.find(p => p.port_name === selectedPort.value)) {
        selectedPort.value = result[0]?.port_name ?? ''
      }
      if (result.length === 0) {
        error.value = '未找到串口设备'
      }
    } catch (e) {
      error.value = String(e)
    } finally {
      isLoading.value = false
    }
  }

  function portLabel(p: PortInfo): string {
    const parts = [p.port_name]
    if (p.product) parts.push(p.product)
    else if (p.manufacturer) parts.push(p.manufacturer)
    if (p.vid && p.pid) parts.push(`[${p.vid}:${p.pid}]`)
    return parts.join(' — ')
  }

  return { ports, selectedPort, baudRate, isLoading, error, refreshPorts, portLabel }
})
