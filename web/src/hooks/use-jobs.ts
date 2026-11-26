import { useCallback, useRef, useState } from 'react'
import { jxlSize, toRgba, webpSize, zipSize } from '@/lib/baselines'
import { compress, decompress, readImage, sha256 } from '@/lib/kolmo'
import type { ImageInfo } from '@/lib/protocol'

export type Measure = { state: 'idle' } | { state: 'running' } | { state: 'done'; bytes: number; note?: string } | { state: 'error'; message: string }

export interface Job {
  id: number
  kind: 'compress' | 'decompress'
  name: string
  inputBytes: number
  previewUrl?: string
  status: 'running' | 'done' | 'error'
  stage: string
  fraction: number
  error?: string
  info?: ImageInfo
  stripes?: number
  seconds?: number
  verifySeconds?: number
  kolmo?: Uint8Array
  png?: Uint8Array
  originalHash?: string
  decodedHash?: string
  verified?: 'pending' | 'ok' | 'mismatch'
  original?: { state: 'match' | 'differs' | 'checking' | 'error'; name: string; message?: string }
  zip: Measure
  webp: Measure
  jxl: Measure
  samples?: Uint8Array
}

const BIG_FOR_BASELINES = 6_000_000

export function useJobs() {
  const [jobs, setJobs] = useState<Job[]>([])
  const nextId = useRef(1)

  const update = useCallback((id: number, patch: Partial<Job> | ((j: Job) => Partial<Job>)) => {
    setJobs((js) => js.map((j) => (j.id === id ? { ...j, ...(typeof patch === 'function' ? patch(j) : patch) } : j)))
  }, [])

  const runCompress = useCallback(
    async (id: number, file: File) => {
      try {
        const buf = await file.arrayBuffer()
        const res = await compress(buf, (p) => update(id, { stage: p.stage, fraction: p.fraction }))
        const originalHash = await sha256(res.samples)
        update(id, {
          info: res.info,
          kolmo: res.kolmo,
          stripes: res.stripes,
          seconds: res.seconds,
          originalHash,
          verified: 'pending',
          stage: 'Verifying round trip',
          fraction: 0,
          samples: res.info.depth === 8 ? res.samples : undefined,
        })
        if (res.info.depth === 8) {
          update(id, { zip: { state: 'running' } })
          zipSize(res.samples, res.info.width, res.info.height, res.info.channels, res.info.depth)
            .then((bytes) => update(id, { zip: { state: 'done', bytes } }))
            .catch((e: Error) => update(id, { zip: { state: 'error', message: e.message } }))
        }
        const dec = await decompress(res.kolmo, false, (p) => update(id, { stage: 'Verifying round trip', fraction: p.fraction }), 'Verifying round trip')
        const decodedHash = await sha256(dec.samples)
        update(id, {
          status: 'done',
          stage: 'Done',
          fraction: 1,
          decodedHash,
          verifySeconds: dec.seconds,
          verified: decodedHash === originalHash ? 'ok' : 'mismatch',
        })
      } catch (e) {
        update(id, { status: 'error', error: e instanceof Error ? e.message : String(e) })
      }
    },
    [update],
  )

  const runDecompress = useCallback(
    async (id: number, file: File) => {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer())
        const res = await decompress(bytes, true, (p) => update(id, { stage: p.stage, fraction: p.fraction }))
        const decodedHash = await sha256(res.samples)
        const previewUrl = URL.createObjectURL(new Blob([res.png as BlobPart], { type: 'image/png' }))
        update(id, {
          status: 'done',
          stage: 'Done',
          fraction: 1,
          info: res.info,
          png: res.png,
          kolmo: bytes,
          stripes: res.stripes,
          seconds: res.seconds,
          decodedHash,
          verified: 'ok',
          previewUrl,
        })
      } catch (e) {
        update(id, { status: 'error', error: e instanceof Error ? e.message : String(e) })
      }
    },
    [update],
  )

  const addFiles = useCallback(
    (files: FileList | File[]) => {
      const list = Array.from(files)
      const created: Job[] = []
      for (const file of list) {
        const id = nextId.current++
        const isKolmo = /\.kolmo$/i.test(file.name)
        const job: Job = {
          id,
          kind: isKolmo ? 'decompress' : 'compress',
          name: file.name,
          inputBytes: file.size,
          previewUrl: isKolmo ? undefined : URL.createObjectURL(file),
          status: 'running',
          stage: isKolmo ? 'Decompressing' : 'Reading image',
          fraction: 0,
          zip: { state: 'idle' },
          webp: { state: 'idle' },
          jxl: { state: 'idle' },
        }
        created.push(job)
      }
      setJobs((js) => [...created.slice().reverse(), ...js])
      created.forEach((job, i) => {
        if (job.kind === 'compress') void runCompress(job.id, list[i])
        else void runDecompress(job.id, list[i])
      })
    },
    [runCompress, runDecompress],
  )

  const compareFormats = useCallback(
    async (job: Job) => {
      if (!job.info || !job.samples) return
      const { width, height, channels } = job.info
      const pixels = width * height
      const rgba = toRgba(job.samples, width, height, channels)
      const effort = pixels <= 2_000_000 ? 9 : 7
      update(job.id, { webp: { state: 'running' }, jxl: { state: 'running' } })
      try {
        const bytes = await webpSize(rgba, width, height)
        update(job.id, { webp: { state: 'done', bytes, note: 'lossless, method 6' } })
      } catch (e) {
        update(job.id, { webp: { state: 'error', message: e instanceof Error ? e.message : String(e) } })
      }
      try {
        const bytes = await jxlSize(rgba, width, height, effort)
        update(job.id, { jxl: { state: 'done', bytes, note: `lossless, effort ${effort}` } })
      } catch (e) {
        update(job.id, { jxl: { state: 'error', message: e instanceof Error ? e.message : String(e) } })
      }
    },
    [update],
  )

  const checkOriginal = useCallback(
    async (job: Job, file: File) => {
      update(job.id, { original: { state: 'checking', name: file.name } })
      try {
        const img = await readImage(await file.arrayBuffer())
        const h = await sha256(img.samples)
        update(job.id, { original: { state: h === job.decodedHash ? 'match' : 'differs', name: file.name } })
      } catch (e) {
        update(job.id, { original: { state: 'error', name: file.name, message: e instanceof Error ? e.message : String(e) } })
      }
    },
    [update],
  )

  const remove = useCallback((id: number) => {
    setJobs((js) => {
      const j = js.find((x) => x.id === id)
      if (j?.previewUrl) URL.revokeObjectURL(j.previewUrl)
      return js.filter((x) => x.id !== id)
    })
  }, [])

  return { jobs, addFiles, compareFormats, checkOriginal, remove, bigForBaselines: BIG_FOR_BASELINES }
}
