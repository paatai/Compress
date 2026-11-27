import data from '@/data/jpeg-benchmark.json'
import speedData from '@/data/jpeg-speed.json'
import { cn } from '@/lib/utils'

interface Group {
  id: string
  label: string
  files: number
  megabytes: number
}
interface Codec {
  id: string
  label: string
  saved: Record<string, number>
  fails: number
  komla?: boolean
}
interface JpegBenchData {
  groups: Group[]
  codecs: Codec[]
}

const bench = data as JpegBenchData

interface SpeedRow {
  id: string
  label: string
  threads: '1t' | 'mt'
  saved: number
  enc: number
  dec: number
  enc_mib: number
  dec_mib: number
  fails: number
}
const speed = speedData as { set: string; files: number; bytes: number; rows: SpeedRow[] }

/** Size, speed and peak memory of every tool, one thread and all cores. */
function SpeedTable() {
  if (!speed.rows.length) return null
  const groups: [SpeedRow['threads'], string][] = [
    ['1t', 'One thread'],
    ['mt', 'All cores (8)'],
  ]
  return (
    <div className="space-y-3">
      <h3 className="text-lg font-semibold tracking-tight">Size, speed and memory</h3>
      <p className="max-w-3xl text-sm text-muted-foreground">
        {speed.files} JPEGs ({(speed.bytes / 1e6).toFixed(0)} MB) drawn at random for the final evaluation and run once, after Komla's code was frozen: every tool one
        process at a time on the same machine, all tools on a file before the next. Speed is megabytes of JPEG per second for the whole process; memory is the largest
        peak of any file. Komla's fast mode is the fastest and uses the least memory in every column; packJPG and the single-threaded Rust port of Lepton make these
        files 0.3 and 0.2 points smaller, and paq8px much smaller at a thousandth of the speed.
      </p>
      <div className="overflow-x-auto rounded-xl border">
        <table className="w-full min-w-[44rem] text-sm">
          <thead className="bg-muted/50 text-left">
            <tr>
              <th className="px-3 py-3 font-medium">Tool</th>
              <th className="px-3 py-3 text-right font-medium">Smaller</th>
              <th className="px-3 py-3 text-right font-medium">Compress</th>
              <th className="px-3 py-3 text-right font-medium">Decompress</th>
              <th className="px-3 py-3 text-right font-medium">Memory, compress</th>
              <th className="px-3 py-3 text-right font-medium">Memory, decompress</th>
            </tr>
          </thead>
          {groups.map(([th, title]) => (
            <tbody key={th}>
              <tr className="border-t bg-muted/30">
                <td colSpan={6} className="px-3 py-2 text-xs font-medium text-muted-foreground">
                  {title}
                </td>
              </tr>
              {speed.rows
                .filter((r) => r.threads === th)
                .map((r) => (
                  <tr key={r.id + th} className={cn('border-t', r.id === 'komla-fast' && 'bg-emerald-500/5 font-semibold')}>
                    <td className="px-3 py-2.5 whitespace-nowrap">{r.label}</td>
                    <td className="px-3 py-2.5 text-right tabular-nums">{r.saved.toFixed(1)}%</td>
                    <td className="px-3 py-2.5 text-right tabular-nums">{r.enc.toFixed(r.enc < 1 ? 2 : 1)} MB/s</td>
                    <td className="px-3 py-2.5 text-right tabular-nums">{r.dec.toFixed(r.dec < 1 ? 2 : 1)} MB/s</td>
                    <td className="px-3 py-2.5 text-right tabular-nums">{Math.round(r.enc_mib)} MiB</td>
                    <td className="px-3 py-2.5 text-right tabular-nums">{Math.round(r.dec_mib)} MiB</td>
                  </tr>
                ))}
            </tbody>
          ))}
        </table>
      </div>
      <p className="text-xs text-muted-foreground">
        JPEG XL: cjxl --lossless_jpeg=1 at its default effort (7), djxl; brunsli and packJPG have no threads; paq8px finished only part of the set in the time
        available. Development and test sets, failures, commands and per-group sizes: bench/results/JPEG_SPEED_REPORT.md.
      </p>
    </div>
  )
}

export function JpegBenchmark() {
  if (!bench.codecs.length) return null
  const cols = [...bench.groups.map((g) => g.id), 'total']
  const best: Record<string, number> = {}
  for (const c of cols) best[c] = Math.max(...bench.codecs.filter((x) => !x.komla && x.saved[c] !== undefined).map((x) => x.saved[c]))
  const komla = bench.codecs.find((c) => c.komla)
  const files = bench.groups.reduce((a, g) => a + g.files, 0)
  const mb = bench.groups.reduce((a, g) => a + g.megabytes, 0)
  return (
    <section id="jpeg-benchmark" className="scroll-mt-20 space-y-6">
      <div className="space-y-2">
        <h2 className="text-2xl font-semibold tracking-tight">JPEG recompression benchmark</h2>
        <p className="max-w-3xl text-muted-foreground">
          How much smaller than the original JPEG files (higher is better), on a held-out test set of {files} JPEGs ({mb.toFixed(0)} MB): Kodak images encoded at several
          qualities, and camera photos, progressive, grayscale and screenshot JPEGs from Wikimedia Commons. Every output was decompressed again and compared with the
          original file byte for byte; a tool that rejects a file counts it at its original size.
        </p>
      </div>
      <div className="overflow-x-auto rounded-xl border">
        <table className="w-full min-w-[52rem] text-sm">
          <thead className="bg-muted/50 text-left">
            <tr>
              <th className="px-3 py-3 font-medium">Tool</th>
              {bench.groups.map((g) => (
                <th key={g.id} className="px-3 py-3 text-right font-medium">
                  <div>{g.label}</div>
                  <div className="text-xs font-normal text-muted-foreground">{g.files} files</div>
                </th>
              ))}
              <th className="px-3 py-3 text-right font-medium">Total</th>
              <th className="px-3 py-3 text-right font-medium">Rejected</th>
            </tr>
          </thead>
          <tbody>
            {bench.codecs.map((codec) => (
              <tr key={codec.id} className={cn('border-t', codec.komla && 'bg-emerald-500/5 font-semibold')}>
                <td className="px-3 py-2.5 whitespace-nowrap">{codec.label}</td>
                {cols.map((c) => {
                  const v = codec.saved[c]
                  return (
                    <td key={c} className={cn('px-3 py-2.5 text-right tabular-nums', !codec.komla && v === best[c] && 'font-semibold')}>
                      {v === undefined ? '—' : `${v.toFixed(1)}%`}
                    </td>
                  )
                })}
                <td className="px-3 py-2.5 text-right tabular-nums">{codec.fails}</td>
              </tr>
            ))}
          </tbody>
          {komla && (
            <tfoot>
              <tr className="border-t bg-muted/30 text-xs text-muted-foreground">
                <td className="px-3 py-2.5">Komla vs best other</td>
                {cols.map((c) => (
                  <td key={c} className="px-3 py-2.5 text-right tabular-nums">
                    {komla.saved[c] !== undefined ? `${komla.saved[c] - best[c] >= 0 ? '+' : ''}${(komla.saved[c] - best[c]).toFixed(1)}` : '—'}
                  </td>
                ))}
                <td />
              </tr>
            </tfoot>
          )}
        </table>
      </div>
      <p className="text-xs text-muted-foreground">
        Points of savings, Komla's maximum compression mode; the last row is the difference to the best other tool in each column. Tools: libjxl 0.13 (cjxl
        --lossless_jpeg=1 -e 9), brunsli, Dropbox Lepton and its Rust port, packJPG 2.5n, paq8px v217 (-8). Full tables, speeds and commands:
        bench/results/JPEG_REPORT.md.
      </p>
      <SpeedTable />
    </section>
  )
}
