import type { BaselineRequest } from './protocol'

let worker: Worker | null = null
let nextId = 1
const pending = new Map<number, { resolve: (n: number) => void; reject: (e: Error) => void }>()

function getWorker(): Worker {
  if (!worker) {
    worker = new Worker(new URL('../workers/baseline.worker.ts', import.meta.url), { type: 'module' })
    worker.onmessage = (ev) => {
      const m = ev.data as { id: number; ok: boolean; size?: number; message?: string }
      const p = pending.get(m.id)
      if (!p) return
      pending.delete(m.id)
      if (m.ok) p.resolve(m.size!)
      else p.reject(new Error(m.message))
    }
    worker.onerror = (ev) => {
      for (const p of pending.values()) p.reject(new Error(ev.message || 'baseline worker failed'))
      pending.clear()
      worker?.terminate()
      worker = null
    }
  }
  return worker
}

function run(req: BaselineRequest, transfer: Transferable[] = []): Promise<number> {
  const w = getWorker()
  const id = nextId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    w.postMessage({ ...req, id }, transfer)
  })
}

export function toRgba(samples: Uint8Array, w: number, h: number, nc: number): Uint8ClampedArray {
  const out = new Uint8ClampedArray(w * h * 4)
  for (let i = 0, s = 0, o = 0; i < w * h; i++, s += nc, o += 4) {
    if (nc <= 2) {
      out[o] = out[o + 1] = out[o + 2] = samples[s]
      out[o + 3] = nc === 2 ? samples[s + 1] : 255
    } else {
      out[o] = samples[s]
      out[o + 1] = samples[s + 1]
      out[o + 2] = samples[s + 2]
      out[o + 3] = nc === 4 ? samples[s + 3] : 255
    }
  }
  return out
}

export function zipSize(samples: Uint8Array, width: number, height: number, channels: number, depth: number) {
  const copy = samples.slice()
  return run({ op: 'zip', samples: copy, width, height, channels, depth }, [copy.buffer])
}

export function webpSize(rgba: Uint8ClampedArray, width: number, height: number) {
  const copy = rgba.slice()
  return run({ op: 'webp', rgba: copy, width, height }, [copy.buffer])
}

export function jxlSize(rgba: Uint8ClampedArray, width: number, height: number, effort: number) {
  const copy = rgba.slice()
  return run({ op: 'jxl', rgba: copy, width, height, effort }, [copy.buffer])
}
