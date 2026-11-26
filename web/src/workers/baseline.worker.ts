/// <reference lib="webworker" />
import { zipSync } from 'fflate'
import type { BaselineRequest } from '../lib/protocol'

const ctx = self as unknown as DedicatedWorkerGlobalScope

/** Uncompressed BMP with the same pixels, as a user would zip it. */
function makeBmp(samples: Uint8Array, w: number, h: number, nc: number): Uint8Array {
  const gray = nc === 1
  const bpp = gray ? 8 : nc === 3 ? 24 : 32
  const rowBytes = Math.ceil((w * bpp) / 32) * 4
  const palette = gray ? 1024 : 0
  const off = 14 + 40 + palette
  const size = off + rowBytes * h
  const out = new Uint8Array(size)
  const v = new DataView(out.buffer)
  out[0] = 0x42
  out[1] = 0x4d
  v.setUint32(2, size, true)
  v.setUint32(10, off, true)
  v.setUint32(14, 40, true)
  v.setInt32(18, w, true)
  v.setInt32(22, h, true)
  v.setUint16(26, 1, true)
  v.setUint16(28, bpp, true)
  v.setUint32(34, rowBytes * h, true)
  if (gray) {
    v.setUint32(46, 256, true)
    for (let i = 0; i < 256; i++) out.set([i, i, i, 0], 54 + i * 4)
  }
  for (let y = 0; y < h; y++) {
    let o = off + (h - 1 - y) * rowBytes
    let s = y * w * nc
    for (let x = 0; x < w; x++, s += nc) {
      if (nc === 1) out[o++] = samples[s]
      else if (nc === 2) {
        out[o++] = samples[s]
        out[o++] = samples[s]
        out[o++] = samples[s]
        out[o++] = samples[s + 1]
      } else {
        out[o++] = samples[s + 2]
        out[o++] = samples[s + 1]
        out[o++] = samples[s]
        if (nc === 4) out[o++] = samples[s + 3]
      }
    }
  }
  return out
}

async function handle(req: BaselineRequest): Promise<number> {
  switch (req.op) {
    case 'zip': {
      const bmp = makeBmp(req.samples, req.width, req.height, req.channels === 2 ? 2 : req.channels)
      return zipSync({ 'image.bmp': [bmp, { level: 9 }] }).length
    }
    case 'webp': {
      const { default: encode } = await import('@jsquash/webp/encode')
      const out = await encode(new ImageData(req.rgba as Uint8ClampedArray<ArrayBuffer>, req.width, req.height), {
        lossless: 1,
        method: 6,
        quality: 100,
        exact: 1,
      })
      return out.byteLength
    }
    case 'jxl': {
      const { default: encode } = await import('@jsquash/jxl/encode')
      const out = await encode(new ImageData(req.rgba as Uint8ClampedArray<ArrayBuffer>, req.width, req.height), {
        lossless: true,
        effort: req.effort,
      })
      return out.byteLength
    }
  }
}

ctx.onmessage = async (ev: MessageEvent<BaselineRequest & { id: number }>) => {
  try {
    const size = await handle(ev.data)
    ctx.postMessage({ id: ev.data.id, ok: true, size })
  } catch (e) {
    ctx.postMessage({ id: ev.data.id, ok: false, message: e instanceof Error ? e.message : String(e) })
  }
}
