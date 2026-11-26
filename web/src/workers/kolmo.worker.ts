/// <reference lib="webworker" />
import wasmUrl from '../wasm/kolmo.wasm?url'
import type { ImageInfo, WorkerRequest } from '../lib/protocol'

interface KolmoExports {
  memory: WebAssembly.Memory
  kolmo_alloc(len: number): number
  kolmo_free(ptr: number, len: number): void
  kolmo_result_ptr(): number
  kolmo_result_len(): number
  kolmo_result_clear(): void
  kolmo_error_ptr(): number
  kolmo_error_len(): number
  [name: string]: unknown
}

const ctx = self as unknown as DedicatedWorkerGlobalScope
let currentId = 0
let lastProgress = 0
let exportsPromise: Promise<KolmoExports> | null = null

function load(): Promise<KolmoExports> {
  exportsPromise ??= (async () => {
    const imports = {
      env: {
        kolmo_progress: (fraction: number) => {
          const now = performance.now()
          if (fraction >= 1 || now - lastProgress > 80) {
            lastProgress = now
            ctx.postMessage({ id: currentId, type: 'progress', fraction })
          }
        },
      },
    }
    const response = fetch(wasmUrl)
    let instance: WebAssembly.Instance
    try {
      instance = (await WebAssembly.instantiateStreaming(response, imports)).instance
    } catch {
      const bytes = await (await fetch(wasmUrl)).arrayBuffer()
      instance = (await WebAssembly.instantiate(bytes, imports)).instance
    }
    return instance.exports as unknown as KolmoExports
  })()
  return exportsPromise
}

/** Copies inputs into WASM memory, calls `fn(ptr, len, ..., extra...)`, returns a copy of the result. */
function call(k: KolmoExports, fn: string, inputs: Uint8Array[], extra: number[] = []): Uint8Array {
  const args: number[] = []
  const allocs: [number, number][] = []
  for (const b of inputs) {
    const p = k.kolmo_alloc(b.length)
    new Uint8Array(k.memory.buffer, p, b.length).set(b)
    allocs.push([p, b.length])
    args.push(p, b.length)
  }
  const rc = (k[fn] as (...a: number[]) => number)(...args, ...extra)
  for (const [p, n] of allocs) k.kolmo_free(p, n)
  if (rc !== 0) {
    const msg = new TextDecoder().decode(new Uint8Array(k.memory.buffer, k.kolmo_error_ptr(), k.kolmo_error_len()))
    throw new Error(msg)
  }
  const out = new Uint8Array(k.memory.buffer, k.kolmo_result_ptr(), k.kolmo_result_len()).slice()
  k.kolmo_result_clear()
  return out
}

function readInfo(v: DataView, o: number): ImageInfo {
  return {
    width: v.getUint32(o, true),
    height: v.getUint32(o + 4, true),
    channels: v.getUint8(o + 8),
    depth: v.getUint8(o + 9),
    codedChannels: v.getUint8(o + 10),
    source: v.getUint8(o + 11) === 1 ? 'bmp' : 'png',
    mode: v.getUint32(o + 12, true),
    paletteSize: v.getUint32(o + 16, true),
  }
}

async function handle(req: WorkerRequest): Promise<{ result: unknown; transfer: Transferable[] }> {
  const k = await load()
  switch (req.op) {
    case 'prepare': {
      const r = call(k, 'kolmo_prepare', [new Uint8Array(req.file)])
      const v = new DataView(r.buffer)
      let o = 0
      const headerLen = v.getUint32(o, true)
      o += 4
      const header = r.slice(o, o + headerLen)
      o += headerLen
      const n = v.getUint32(o, true)
      o += 4
      const stripes: [number, number][] = []
      for (let i = 0; i < n; i++, o += 8) stripes.push([v.getUint32(o, true), v.getUint32(o + 4, true)])
      const info = readInfo(v, o)
      o += 20
      const sampleLen = v.getUint32(o, true)
      o += 4
      const samples = r.slice(o, o + sampleLen)
      o += sampleLen
      const coded = r.slice(o)
      return { result: { header, stripes, info, samples, coded }, transfer: [header.buffer, samples.buffer, coded.buffer] }
    }
    case 'encodeStripe': {
      const s = call(k, 'kolmo_encode_stripe', [req.header, req.rows], [req.height])
      return { result: s, transfer: [s.buffer] }
    }
    case 'parse': {
      const r = call(k, 'kolmo_parse', [req.file])
      const v = new DataView(r.buffer)
      const headerLen = v.getUint32(0, true)
      const n = v.getUint32(4, true)
      const stripes = []
      let o = 8
      for (let i = 0; i < n; i++, o += 16) {
        stripes.push({ offset: v.getUint32(o, true), length: v.getUint32(o + 4, true), y0: v.getUint32(o + 8, true), y1: v.getUint32(o + 12, true) })
      }
      return { result: { headerLen, stripes, info: readInfo(v, o) }, transfer: [] }
    }
    case 'decodeStripe': {
      const c = call(k, 'kolmo_decode_stripe', [req.header, req.data], [req.height])
      return { result: c, transfer: [c.buffer] }
    }
    case 'finish': {
      const r = call(k, 'kolmo_finish', [req.header, req.coded], [req.wantPng ? 1 : 0])
      const pngLen = new DataView(r.buffer).getUint32(0, true)
      const png = r.slice(4, 4 + pngLen)
      const samples = r.slice(4 + pngLen)
      return { result: { png, samples }, transfer: [png.buffer, samples.buffer] }
    }
    case 'readImage': {
      const r = call(k, 'kolmo_read_image', [new Uint8Array(req.file)])
      const v = new DataView(r.buffer)
      const info = { width: v.getUint32(0, true), height: v.getUint32(4, true), channels: r[8], depth: r[9] }
      const samples = r.slice(12)
      return { result: { info, samples }, transfer: [samples.buffer] }
    }
  }
}

ctx.onmessage = async (ev: MessageEvent<WorkerRequest & { id: number }>) => {
  const req = ev.data
  currentId = req.id
  lastProgress = 0
  try {
    const { result, transfer } = await handle(req)
    ctx.postMessage({ id: req.id, type: 'result', result }, transfer)
  } catch (e) {
    ctx.postMessage({ id: req.id, type: 'error', message: e instanceof Error ? e.message : String(e) })
  }
}
