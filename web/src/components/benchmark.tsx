import data from '@/data/benchmark.json'
import { cn } from '@/lib/utils'

interface Category {
  id: string
  label: string
  images: number
  megapixels: number
}
interface Codec {
  id: string
  label: string
  bpp: Record<string, number>
  kolmo?: boolean
}
interface BenchData {
  categories: Category[]
  codecs: Codec[]
  notes: string[]
}

const bench = data as BenchData

export function Benchmark() {
  if (!bench.codecs.length) return null
  const cols = [...bench.categories.map((c) => c.id), 'total']
  const best: Record<string, number> = {}
  for (const c of cols) best[c] = Math.min(...bench.codecs.filter((x) => !x.kolmo && x.bpp[c] !== undefined).map((x) => x.bpp[c]))
  const kolmo = bench.codecs.find((c) => c.kolmo)
  return (
    <section id="benchmark" className="scroll-mt-20 space-y-6">
      <div className="space-y-2">
        <h2 className="text-2xl font-semibold tracking-tight">Benchmark</h2>
        <p className="max-w-3xl text-muted-foreground">
          Bits per pixel on a public test set (lower is better). Every result was decoded again and hash-checked against the original pixels. Kolmo was tuned
          only on a separate set of images.
        </p>
      </div>
      <div className="overflow-x-auto rounded-xl border">
        <table className="w-full min-w-[40rem] text-sm">
          <thead className="bg-muted/50 text-left">
            <tr>
              <th className="px-4 py-3 font-medium">Codec</th>
              {bench.categories.map((c) => (
                <th key={c.id} className="px-4 py-3 text-right font-medium">
                  <div>{c.label}</div>
                  <div className="text-xs font-normal text-muted-foreground">
                    {c.images} images, {c.megapixels.toFixed(1)} MP
                  </div>
                </th>
              ))}
              <th className="px-4 py-3 text-right font-medium">Total</th>
            </tr>
          </thead>
          <tbody>
            {bench.codecs.map((codec) => (
              <tr key={codec.id} className={cn('border-t', codec.kolmo && 'bg-emerald-500/5 font-semibold')}>
                <td className="px-4 py-2.5">{codec.label}</td>
                {cols.map((c) => {
                  const v = codec.bpp[c]
                  return (
                    <td key={c} className={cn('px-4 py-2.5 text-right tabular-nums', !codec.kolmo && v === best[c] && 'font-semibold')}>
                      {v === undefined ? '—' : v.toFixed(3)}
                    </td>
                  )
                })}
              </tr>
            ))}
          </tbody>
          {kolmo && (
            <tfoot>
              <tr className="border-t bg-muted/30 text-xs text-muted-foreground">
                <td className="px-4 py-2.5">Kolmo vs best other</td>
                {cols.map((c) => (
                  <td key={c} className="px-4 py-2.5 text-right tabular-nums">
                    {kolmo.bpp[c] !== undefined ? `${((1 - kolmo.bpp[c] / best[c]) * 100).toFixed(1)}% smaller` : '—'}
                  </td>
                ))}
              </tr>
            </tfoot>
          )}
        </table>
      </div>
      <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
        {bench.notes.map((n) => (
          <li key={n}>{n}</li>
        ))}
      </ul>
    </section>
  )
}
