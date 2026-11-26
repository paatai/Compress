/// <reference lib="webworker" />
import wasmUrl from '../wasm/komla.wasm?url'
import type { ImageInfo, JpegInfo, WorkerRequest } from '../lib/protocol'

interface KomlaExports {
  memory: WebAssembly.Memory
  komla_alloc(len: number): number
  komla_free(ptr: number, len: number): void
  komla_result_ptr(): number
  komla_result_len(): number
  komla_result_clear(): void
  komla_error_ptr(): number
  komla_error_len(): number
  [name: string]: unknown
}

const ctx = self as unknown as DedicatedWorkerGlobalScope
let currentId = 0
let lastProgress = 0
let exportsPromise: Promise<KomlaExports> | null = null

function load(): Promise<KomlaExports> {
  exportsPromise ??= (async () => {
    const imports = {
      env: {
        komla_progress: (fraction: number) => {
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
    return instance.exports as unknown as KomlaExports
  })()
  return exportsPromise
}

/** Copies inputs into WASM memory, calls `fn(ptr, len, ..., extra...)`, returns a copy of the result. */
function call(k: KomlaExports, fn: string, inputs: Uint8Array[], extra: number[] = []): Uint8Array {
  const args: number[] = []
  const allocs: [number, number][] = []
  for (const b of inputs) {
    const p = k.komla_alloc(b.length)
    new Uint8Array(k.memory.buffer, p, b.length).set(b)
    allocs.push([p, b.length])
    args.push(p, b.length)
  }
  const rc = (k[fn] as (...a: number[]) => number)(...args, ...extra)
  for (const [p, n] of allocs) k.komla_free(p, n)
  if (rc !== 0) {
    const msg = new TextDecoder().decode(new Uint8Array(k.memory.buffer, k.komla_error_ptr(), k.komla_error_len()))
    throw new Error(msg)
  }
  const out = new Uint8Array(k.memory.buffer, k.komla_result_ptr(), k.komla_result_len()).slice()
  k.komla_result_clear()
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

/** Reads the JPEG info block written by `jpeg_info_block`; returns it and the next offset. */
function readJpegInfo(r: Uint8Array, o: number): [JpegInfo, number] {
  const v = new DataView(r.buffer, r.byteOffset, r.byteLength)
  const td = new TextDecoder()
  const width = v.getUint32(o, true)
  const height = v.getUint32(o + 4, true)
  const components = r[o + 8]
  const progressive = r[o + 9] !== 0
  const mode = r[o + 10]
  const stripes = v.getUint32(o + 12, true)
  o += 16
  const strs: string[] = []
  for (let i = 0; i < 2; i++) {
    const n = v.getUint32(o, true)
    strs.push(td.decode(r.subarray(o + 4, o + 4 + n)))
    o += 4 + n
  }
  return [{ width, height, components, progressive, mode, stripes, sampling: strs[0], note: strs[1] }, o]
}

async function handle(req: WorkerRequest): Promise<{ result: unknown; transfer: Transferable[] }> {
  const k = await load()
  switch (req.op) {
    case 'prepare': {
      const r = call(k, 'komla_prepare', [new Uint8Array(req.file)])
      const v = new DataView(r.buffer)
      let o = 0
      const nCand = v.getUint32(o, true)
      o += 4
      const candidates = []
      for (let c = 0; c < nCand; c++) {
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
        candidates.push({ header, stripes, info })
      }
      const sampleLen = v.getUint32(o, true)
      o += 4
      const samples = r.slice(o, o + sampleLen)
      o += sampleLen
      const coded = r.slice(o)
      return { result: { candidates, samples, coded }, transfer: [samples.buffer, coded.buffer] }
    }
    case 'encodeStripe': {
      const s = call(k, 'komla_encode_stripe', [req.header, req.rows], [req.height])
      return { result: s, transfer: [s.buffer] }
    }
    case 'parse': {
      const r = call(k, 'komla_parse', [req.file])
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
      const c = call(k, 'komla_decode_stripe', [req.header, req.data], [req.height])
      return { result: c, transfer: [c.buffer] }
    }
    case 'finish': {
      const r = call(k, 'komla_finish', [req.header, req.coded], [req.wantPng ? 1 : 0])
      const pngLen = new DataView(r.buffer).getUint32(0, true)
      const png = r.slice(4, 4 + pngLen)
      const samples = r.slice(4 + pngLen)
      return { result: { png, samples }, transfer: [png.buffer, samples.buffer] }
    }
    case 'jpegPrepare': {
      const r = call(k, 'komla_jpeg_prepare', [new Uint8Array(req.file)])
      const v = new DataView(r.buffer)
      const [info, start] = readJpegInfo(r, 0)
      let o = start
      const hl = v.getUint32(o, true)
      const header = r.slice(o + 4, o + 4 + hl)
      o += 4 + hl
      const bl = v.getUint32(o, true)
      const blob = r.slice(o + 4, o + 4 + bl)
      o += 4 + bl
      const n = v.getUint32(o, true)
      o += 4
      const stripes: Uint8Array[] = []
      for (let i = 0; i < n; i++) {
        const len = v.getUint32(o, true)
        stripes.push(r.slice(o + 4, o + 4 + len))
        o += 4 + len
      }
      return { result: { info, header, blob, stripes }, transfer: [header.buffer, blob.buffer, ...stripes.map((s) => s.buffer)] }
    }
    case 'jpegEncodeStripe': {
      const s = call(k, 'komla_jpeg_encode_stripe', [req.blob, req.coefs], [req.k])
      return { result: s, transfer: [s.buffer] }
    }
    case 'jpegParse': {
      const r = call(k, 'komla_jpeg_parse', [req.file])
      const v = new DataView(r.buffer)
      const [info, start] = readJpegInfo(r, 0)
      let o = start
      const bl = v.getUint32(o, true)
      const blob = r.slice(o + 4, o + 4 + bl)
      o += 4 + bl
      const n = v.getUint32(o, true)
      o += 4
      const stripes = []
      for (let i = 0; i < n; i++, o += 8) stripes.push({ offset: v.getUint32(o, true), length: v.getUint32(o + 4, true) })
      return { result: { info, blob, stripes }, transfer: [blob.buffer] }
    }
    case 'jpegDecodeStripe': {
      const c = call(k, 'komla_jpeg_decode_stripe', [req.blob, req.data], [req.k])
      return { result: c, transfer: [c.buffer] }
    }
    case 'jpegFinish': {
      const j = call(k, 'komla_jpeg_finish', [req.file, req.coefs])
      return { result: j, transfer: [j.buffer] }
    }
    case 'readImage': {
      const r = call(k, 'komla_read_image', [new Uint8Array(req.file)])
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
