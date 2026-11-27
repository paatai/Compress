import data from '@/data/jpeg-benchmark.json'
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
        Points of savings; the last row is the difference to the best other tool in each column. Tools: libjxl 0.13 (cjxl --lossless_jpeg=1 -e 9), brunsli, Dropbox
        Lepton and its Rust port, packJPG 2.5n, paq8px v217 (-8). Full tables, speeds and commands: bench/results/JPEG_REPORT.md.
      </p>
    </section>
  )
}
