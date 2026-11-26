import type { ImageInfo, ParsedFile, Prepared, WorkerRequest } from './protocol'

type Pending = {
  resolve: (v: unknown) => void
  reject: (e: Error) => void
  onProgress?: (f: number) => void
}

/** A small pool of codec workers; each owns its own WebAssembly instance. */
class Pool {
  private workers: Worker[] = []
  private idle: Worker[] = []
  private queue: { req: WorkerRequest; transfer: Transferable[]; p: Pending }[] = []
  private pending = new Map<number, { p: Pending; w: Worker }>()
  private nextId = 1
  readonly size: number

  constructor(size: number) {
    this.size = size
  }

  private spawn(): Worker {
    const w = new Worker(new URL('../workers/kolmo.worker.ts', import.meta.url), { type: 'module' })
    w.onmessage = (ev) => {
      const msg = ev.data as { id: number; type: string; fraction?: number; result?: unknown; message?: string }
      const entry = this.pending.get(msg.id)
      if (!entry) return
      if (msg.type === 'progress') {
        entry.p.onProgress?.(msg.fraction ?? 0)
        return
      }
      this.pending.delete(msg.id)
      if (msg.type === 'result') entry.p.resolve(msg.result)
      else entry.p.reject(new Error(msg.message ?? 'worker error'))
      this.release(entry.w)
    }
    w.onerror = (ev) => {
      for (const [id, entry] of this.pending) {
        if (entry.w === w) {
          this.pending.delete(id)
          entry.p.reject(new Error(ev.message || 'The codec worker crashed (out of memory?)'))
        }
      }
      this.workers = this.workers.filter((x) => x !== w)
      this.idle = this.idle.filter((x) => x !== w)
      w.terminate()
      this.pump()
    }
    this.workers.push(w)
    return w
  }

  private release(w: Worker) {
    this.idle.push(w)
    this.pump()
  }

  private pump() {
    while (this.queue.length > 0) {
      let w = this.idle.pop()
      if (!w && this.workers.length < this.size) w = this.spawn()
      if (!w) return
      const job = this.queue.shift()!
      const id = this.nextId++
      this.pending.set(id, { p: job.p, w })
      w.postMessage({ ...job.req, id }, job.transfer)
    }
  }

  run<T>(req: WorkerRequest, transfer: Transferable[] = [], onProgress?: (f: number) => void): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      this.queue.push({ req, transfer, p: { resolve: resolve as (v: unknown) => void, reject, onProgress } })
      this.pump()
    })
  }
}

const cores = typeof navigator !== 'undefined' ? navigator.hardwareConcurrency || 4 : 4
export const pool = new Pool(Math.max(2, Math.min(cores, 8)))

function putVarint(out: number[], v: number) {
  while (v >= 0x80) {
    out.push((v & 0x7f) | 0x80)
    v = Math.floor(v / 128)
  }
  out.push(v)
}

export function assemble(header: Uint8Array, streams: Uint8Array[]): Uint8Array {
  const table: number[] = []
  putVarint(table, streams.length)
  for (const s of streams.slice(0, -1)) putVarint(table, s.length)
  const total = header.length + table.length + streams.reduce((a, s) => a + s.length, 0)
  const out = new Uint8Array(total)
  out.set(header, 0)
  out.set(table, header.length)
  let o = header.length + table.length
  for (const s of streams) {
    out.set(s, o)
    o += s.length
  }
  return out
}

export interface Progress {
  stage: string
  fraction: number
}

function stripeProgress(stripes: [number, number][], stage: string, onProgress: (p: Progress) => void) {
  const total = stripes.reduce((a, [y0, y1]) => a + (y1 - y0), 0)
  const done = stripes.map(() => 0)
  return (k: number) => (f: number) => {
    done[k] = f * (stripes[k][1] - stripes[k][0])
    onProgress({ stage, fraction: done.reduce((a, b) => a + b, 0) / total })
  }
}

export async function sha256(data: Uint8Array): Promise<string> {
  const d = await crypto.subtle.digest('SHA-256', data as BufferSource)
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, '0')).join('')
}

export interface CompressResult {
  kolmo: Uint8Array
  info: ImageInfo
  stripes: number
  samples: Uint8Array
  seconds: number
}

export async function compress(file: ArrayBuffer, onProgress: (p: Progress) => void): Promise<CompressResult> {
  const t0 = performance.now()
  onProgress({ stage: 'Reading image', fraction: 0 })
  const prep = await pool.run<Prepared>({ op: 'prepare', file }, [file])
  const { candidates, coded } = prep
  const jobs = candidates.flatMap((c, ci) => c.stripes.map((s) => ({ ci, s })))
  const stage = candidates.length > 1 ? 'Compressing (trying palette and continuous-tone modes)' : 'Compressing'
  const progress = stripeProgress(
    jobs.map((j) => j.s),
    stage,
    onProgress,
  )
  const streams = await Promise.all(
    jobs.map(({ ci, s: [y0, y1] }, k) => {
      const c = candidates[ci]
      const rowBytes = c.info.width * c.info.codedChannels * 2
      const rows = coded.slice(y0 * rowBytes, y1 * rowBytes)
      return pool.run<Uint8Array>({ op: 'encodeStripe', header: c.header, rows, height: y1 - y0 }, [rows.buffer], progress(k))
    }),
  )
  let best: { kolmo: Uint8Array; ci: number } | null = null
  candidates.forEach((c, ci) => {
    const kolmo = assemble(
      c.header,
      jobs.flatMap((j, k) => (j.ci === ci ? [streams[k]] : [])),
    )
    if (!best || kolmo.length < best.kolmo.length) best = { kolmo, ci }
  })
  const chosen = best!
  const cand = candidates[chosen.ci]
  return { kolmo: chosen.kolmo, info: cand.info, stripes: cand.stripes.length, samples: prep.samples, seconds: (performance.now() - t0) / 1000 }
}

export interface DecompressResult {
  png: Uint8Array
  samples: Uint8Array
  info: ImageInfo
  stripes: number
  seconds: number
}

export async function decompress(kolmo: Uint8Array, wantPng: boolean, onProgress: (p: Progress) => void, stage = 'Decompressing'): Promise<DecompressResult> {
  const t0 = performance.now()
  const parsed = await pool.run<ParsedFile>({ op: 'parse', file: kolmo })
  const header = kolmo.slice(0, parsed.headerLen)
  const rows = parsed.stripes.map((s) => [s.y0, s.y1] as [number, number])
  const progress = stripeProgress(rows, stage, onProgress)
  const parts = await Promise.all(
    parsed.stripes.map((s, k) => {
      const data = kolmo.slice(s.offset, s.offset + s.length)
      return pool.run<Uint8Array>({ op: 'decodeStripe', header, data, height: s.y1 - s.y0 }, [data.buffer], progress(k))
    }),
  )
  const coded = new Uint8Array(parts.reduce((a, p) => a + p.length, 0))
  let o = 0
  for (const p of parts) {
    coded.set(p, o)
    o += p.length
  }
  onProgress({ stage: wantPng ? 'Writing PNG' : 'Checking', fraction: 1 })
  const fin = await pool.run<{ png: Uint8Array; samples: Uint8Array }>({ op: 'finish', header, coded, wantPng }, [coded.buffer])
  return { png: fin.png, samples: fin.samples, info: parsed.info, stripes: parsed.stripes.length, seconds: (performance.now() - t0) / 1000 }
}

export async function readImage(file: ArrayBuffer): Promise<{ info: { width: number; height: number; channels: number; depth: number }; samples: Uint8Array }> {
  return pool.run({ op: 'readImage', file }, [file])
}
