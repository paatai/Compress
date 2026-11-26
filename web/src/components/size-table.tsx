import { Loader2 } from 'lucide-react'
import type { Measure } from '@/hooks/use-jobs'
import { formatBpp, formatBytes } from '@/lib/format'
import { cn } from '@/lib/utils'

export interface SizeRow {
  label: string
  detail?: string
  measure: Measure
  highlight?: boolean
}

export function SizeTable({ rows, pixels, kolmoBytes }: { rows: SizeRow[]; pixels: number; kolmoBytes?: number }) {
  const done = rows.flatMap((r) => (r.measure.state === 'done' ? [r.measure.bytes] : []))
  const max = Math.max(1, ...done)
  return (
    <div className="space-y-2.5">
      {rows.map((r) => {
        const m = r.measure
        const bytes = m.state === 'done' ? m.bytes : undefined
        const saving = bytes && kolmoBytes && !r.highlight ? 1 - kolmoBytes / bytes : undefined
        return (
          <div key={r.label} className="grid grid-cols-[minmax(0,9.5rem)_1fr] items-center gap-3 sm:grid-cols-[12rem_1fr_9rem]">
            <div className="min-w-0">
              <div className={cn('truncate text-sm', r.highlight ? 'font-semibold' : 'text-foreground/90')}>{r.label}</div>
              {(r.detail || (m.state === 'done' && m.note)) && (
                <div className="truncate text-xs text-muted-foreground">{m.state === 'done' && m.note ? m.note : r.detail}</div>
              )}
              {saving !== undefined && (
                <div className={cn('text-xs font-medium', saving > 0 ? 'text-emerald-600' : 'text-amber-600')}>
                  Kolmo is {(Math.abs(saving) * 100).toFixed(1)}% {saving > 0 ? 'smaller' : 'larger'}
                </div>
              )}
            </div>
            <div className="flex min-w-0 items-center gap-2">
              <div className="h-2.5 flex-1 overflow-hidden rounded-full bg-muted">
                {bytes !== undefined && (
                  <div
                    className={cn('h-full rounded-full transition-[width] duration-500', r.highlight ? 'bg-emerald-500' : 'bg-foreground/25')}
                    style={{ width: `${Math.max(1.5, (bytes / max) * 100)}%` }}
                  />
                )}
              </div>
              <div className="w-20 shrink-0 text-right text-sm tabular-nums sm:hidden">
                {bytes !== undefined ? formatBytes(bytes) : m.state === 'running' ? '…' : '—'}
              </div>
            </div>
            <div className="hidden text-right text-sm tabular-nums sm:block">
              {m.state === 'running' && <Loader2 className="ml-auto size-4 animate-spin text-muted-foreground" />}
              {m.state === 'idle' && <span className="text-muted-foreground">—</span>}
              {m.state === 'error' && (
                <span className="text-destructive" title={m.message}>
                  failed
                </span>
              )}
              {bytes !== undefined && (
                <div>
                  <div className={cn(r.highlight && 'font-semibold')}>{formatBytes(bytes)}</div>
                  <div className="text-xs text-muted-foreground">{formatBpp(bytes, pixels)}</div>
                </div>
              )}
            </div>
          </div>
        )
      })}
    </div>
  )
}
