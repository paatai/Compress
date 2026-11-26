import { useRef } from 'react'
import { AlertTriangle, CheckCircle2, Download, FileSearch, Loader2, ShieldAlert, ShieldCheck, Trash2, Scale } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Progress } from '@/components/ui/progress'
import type { Job } from '@/hooks/use-jobs'
import { describeChannels, describeJpeg, download, formatBpp, formatBytes, formatSeconds, jpegName, stripExt } from '@/lib/format'
import { JPEG_MODE_RECOMPRESSED } from '@/lib/protocol'
import { SizeTable, type SizeRow } from './size-table'

function modeLabel(mode: number, paletteSize: number) {
  if (mode & 1) return `palette (${paletteSize} colours)`
  if (mode & 4) return 'continuous tone + repeat flags'
  return 'continuous tone'
}

function Hash({ label, value }: { label: string; value?: string }) {
  if (!value) return null
  return (
    <div className="min-w-0">
      <span className="text-muted-foreground">{label} </span>
      <code className="font-mono text-[11px] break-all" title={value}>
        {value.slice(0, 16)}…{value.slice(-8)}
      </code>
    </div>
  )
}

export function JobCard({
  job,
  onCompare,
  onCheckOriginal,
  onRemove,
}: {
  job: Job
  onCompare: (j: Job) => void
  onCheckOriginal: (j: Job, f: File) => void
  onRemove: (id: number) => void
}) {
  const pick = useRef<HTMLInputElement>(null)
  const isJpeg = job.format === 'jpeg'
  const info = job.info
  const jinfo = job.jpegInfo
  const pixels = info ? info.width * info.height : jinfo ? jinfo.width * jinfo.height : 0
  const komlaBytes = job.kind === 'compress' ? job.komla?.length : job.inputBytes
  const srcLabel = job.kind === 'compress' ? (/\.bmp$/i.test(job.name) ? 'Original BMP file' : 'Original PNG file') : 'PNG (written here)'
  const what = isJpeg ? 'the original JPEG file' : 'every pixel'

  const rows: SizeRow[] = []
  if (isJpeg) {
    if (job.kind === 'compress') rows.push({ label: 'Original JPEG', detail: 'your file', measure: { state: 'done', bytes: job.inputBytes } })
    else if (job.jpeg) rows.push({ label: 'JPEG', detail: 'restored original file', measure: { state: 'done', bytes: job.jpeg.length } })
  } else if (job.kind === 'compress') {
    rows.push({ label: srcLabel, measure: { state: 'done', bytes: job.inputBytes } })
    rows.push({ label: 'ZIP −9', detail: 'of the same pixels as BMP', measure: job.zip })
    rows.push({ label: 'WebP lossless', detail: 'libwebp in your browser', measure: job.webp })
    rows.push({ label: 'JPEG XL lossless', detail: 'libjxl in your browser', measure: job.jxl })
  } else if (job.png) {
    rows.push({ label: 'PNG', detail: 'restored file (fast deflate)', measure: { state: 'done', bytes: job.png.length } })
  }
  if (komlaBytes) rows.push({ label: 'Komla', detail: '.komla file', measure: job.komla || job.kind === 'decompress' ? { state: 'done', bytes: komlaBytes } : { state: 'running' }, highlight: true })

  const canCompare = !isJpeg && job.kind === 'compress' && (job.status === 'done' || job.status === 'verifying') && !!job.samples && job.webp.state === 'idle'
  const restored = isJpeg ? job.jpeg : job.png
  const stored = isJpeg && jinfo && jinfo.mode !== JPEG_MODE_RECOMPRESSED

  return (
    <Card className="overflow-hidden">
      <CardContent className="grid gap-5 p-4 sm:p-5 md:grid-cols-[13rem_1fr]">
        <div className="flex flex-col gap-3">
          <div className="flex aspect-square items-center justify-center overflow-hidden rounded-lg border bg-[repeating-conic-gradient(#f4f4f5_0_25%,#fff_0_50%)] bg-[length:16px_16px]">
            {job.previewUrl ? (
              <img src={job.previewUrl} alt={job.name} className="max-h-full max-w-full object-contain" />
            ) : (
              <FileSearch className="size-10 text-muted-foreground" strokeWidth={1.25} />
            )}
          </div>
          {info && (
            <div className="space-y-0.5 text-xs text-muted-foreground">
              <div>
                {info.width} × {info.height} px
              </div>
              <div>{describeChannels(info.channels, info.depth)}</div>
              <div>Mode: {modeLabel(info.mode, info.paletteSize)}</div>
              {job.stripes && job.stripes > 1 && <div>{job.stripes} stripes coded in parallel</div>}
            </div>
          )}
          {jinfo && (
            <div className="space-y-0.5 text-xs text-muted-foreground">
              {jinfo.mode === JPEG_MODE_RECOMPRESSED ? (
                <>
                  <div>
                    {jinfo.width} × {jinfo.height} px
                  </div>
                  <div>{describeJpeg(jinfo)}</div>
                  <div>Mode: lossless JPEG recompression</div>
                  {jinfo.stripes > 1 && <div>{jinfo.stripes} stripes coded in parallel</div>}
                </>
              ) : (
                <div>Mode: stored as is</div>
              )}
            </div>
          )}
        </div>

        <div className="flex min-w-0 flex-col gap-4">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="truncate font-medium" title={job.name}>
                  {job.name}
                </h3>
                <Badge variant="secondary">{job.kind === 'compress' ? 'Compress' : 'Restore'}</Badge>
                {isJpeg && <Badge variant="outline">JPEG</Badge>}
              </div>
              <p className="text-sm text-muted-foreground">
                {formatBytes(job.inputBytes)}
                {job.status !== 'running' && job.status !== 'error' && job.seconds !== undefined && (
                  <>
                    {' · '}
                    {job.kind === 'compress' ? 'compressed' : 'decoded'} in {formatSeconds(job.seconds)}
                    {job.verifySeconds !== undefined && <> · verified in {formatSeconds(job.verifySeconds)}</>}
                  </>
                )}
              </p>
            </div>
            <Button variant="ghost" size="icon" aria-label="Remove" onClick={() => onRemove(job.id)}>
              <Trash2 className="size-4" />
            </Button>
          </div>

          {job.status === 'error' && (
            <div className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
              <AlertTriangle className="mt-0.5 size-4 shrink-0" />
              <span>{job.error}</span>
            </div>
          )}

          {(job.status === 'running' || job.status === 'verifying') && (
            <div className="space-y-2">
              <div className="flex items-center justify-between text-sm">
                <span className="flex items-center gap-2">
                  <Loader2 className="size-4 animate-spin" />
                  {job.stage}…
                </span>
                <span className="tabular-nums text-muted-foreground">{Math.round(job.fraction * 100)}%</span>
              </div>
              <Progress value={job.fraction * 100} />
            </div>
          )}

          {job.status !== 'error' && (info || jinfo) && rows.length > 0 && <SizeTable rows={rows} pixels={pixels} komlaBytes={komlaBytes} />}

          {stored && job.kind === 'compress' && (
            <div className="flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600" />
              <span>
                This JPEG could not be reproduced exactly from its coefficients{jinfo?.note ? ` (${jinfo.note})` : ''}, so it is stored as it is. The .komla file still restores
                it byte for byte.
              </span>
            </div>
          )}

          {(job.status === 'done' || job.status === 'verifying') && (
            <div className="space-y-3">
              {job.kind === 'compress' && job.verified === 'ok' && (
                <div className="flex items-start gap-2 rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-3 text-sm">
                  <ShieldCheck className="mt-0.5 size-4 shrink-0 text-emerald-600" />
                  <div className="min-w-0 space-y-1">
                    <div className="font-medium text-emerald-700 dark:text-emerald-400">Bit-exact round trip verified</div>
                    <div className="text-xs text-muted-foreground">
                      {isJpeg
                        ? 'The .komla file was decoded again here and gives back the original JPEG file, byte for byte.'
                        : 'The .komla file was decoded again here and every pixel matches the original.'}
                    </div>
                    <div className="space-y-0.5 text-xs">
                      <Hash label={isJpeg ? 'SHA-256 original file' : 'SHA-256 original pixels'} value={job.originalHash} />
                      <Hash label={isJpeg ? 'SHA-256 restored file' : 'SHA-256 decoded pixels'} value={job.decodedHash} />
                    </div>
                  </div>
                </div>
              )}
              {job.kind === 'compress' && job.verified === 'mismatch' && (
                <div className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
                  <ShieldAlert className="mt-0.5 size-4 shrink-0" />
                  <span>Round-trip check failed. Please do not rely on this file and report the image.</span>
                </div>
              )}
              {job.kind === 'decompress' && (
                <div className="flex items-start gap-2 rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-3 text-sm">
                  <ShieldCheck className="mt-0.5 size-4 shrink-0 text-emerald-600" />
                  <div className="min-w-0 space-y-1">
                    <div className="font-medium text-emerald-700 dark:text-emerald-400">{isJpeg ? 'Original JPEG restored, checksum verified' : 'Decoded and checksum verified'}</div>
                    <div className="text-xs text-muted-foreground">
                      {isJpeg
                        ? 'The CRC-32 stored in the file matches the restored JPEG. Check against your original file for a full byte-by-byte comparison.'
                        : 'The CRC-32 stored in the file matches the decoded pixels. Check against your original file for a full comparison.'}
                    </div>
                    <div className="text-xs">
                      <Hash label={isJpeg ? 'SHA-256 restored file' : 'SHA-256 decoded pixels'} value={job.decodedHash} />
                    </div>
                    {job.original && (
                      <div className="text-xs">
                        {job.original.state === 'checking' && <span className="text-muted-foreground">Comparing with {job.original.name}…</span>}
                        {job.original.state === 'match' && (
                          <span className="flex items-center gap-1 font-medium text-emerald-700 dark:text-emerald-400">
                            <CheckCircle2 className="size-3.5" /> {isJpeg ? 'Byte-for-byte identical to' : 'Identical pixels to'} {job.original.name}
                          </span>
                        )}
                        {job.original.state === 'differs' && (
                          <span className="font-medium text-destructive">
                            {isJpeg ? 'Differs from' : 'Pixels differ from'} {job.original.name}
                          </span>
                        )}
                        {job.original.state === 'error' && <span className="text-destructive">Could not read {job.original.name}: {job.original.message}</span>}
                      </div>
                    )}
                  </div>
                </div>
              )}

              <div className="flex flex-wrap gap-2">
                {job.kind === 'compress' && job.komla && (
                  <Button onClick={() => download(job.komla!, `${job.name}.komla`, 'application/octet-stream')}>
                    <Download className="size-4" /> Download .komla
                    <span className="opacity-70">({formatBytes(job.komla.length)})</span>
                  </Button>
                )}
                {job.kind === 'decompress' && restored && (
                  <Button
                    onClick={() => (isJpeg ? download(restored, jpegName(job.name), 'image/jpeg') : download(restored, `${stripExt(stripExt(job.name))}.png`, 'image/png'))}
                  >
                    <Download className="size-4" /> {isJpeg ? 'Download JPG' : 'Download PNG'}
                  </Button>
                )}
                {canCompare && (
                  <Button variant="outline" onClick={() => onCompare(job)}>
                    <Scale className="size-4" /> Compare with WebP and JPEG XL
                  </Button>
                )}
                {job.kind === 'decompress' && (
                  <>
                    <Button variant="outline" onClick={() => pick.current?.click()}>
                      <FileSearch className="size-4" /> Check against original…
                    </Button>
                    <input
                      ref={pick}
                      type="file"
                      accept={isJpeg ? '.jpg,.jpeg,.jfif,image/jpeg' : '.png,.bmp,image/png,image/bmp'}
                      className="hidden"
                      onChange={(e) => {
                        const f = e.target.files?.[0]
                        if (f) onCheckOriginal(job, f)
                        e.target.value = ''
                      }}
                    />
                  </>
                )}
              </div>
              {job.kind === 'compress' && job.komla && info && (
                <p className="text-xs text-muted-foreground">
                  {formatBpp(job.komla.length, pixels)} · {((job.komla.length / job.inputBytes) * 100).toFixed(1)}% of the input file · ratio{' '}
                  {((pixels * info.channels * (info.depth / 8)) / job.komla.length).toFixed(2)}:1 versus raw pixels
                </p>
              )}
              {job.kind === 'compress' && job.komla && isJpeg && (
                <p className="text-xs text-muted-foreground">
                  {((1 - job.komla.length / job.inputBytes) * 100).toFixed(1)}% smaller than the JPEG ({formatBytes(job.inputBytes - job.komla.length)} saved){pixels ? ` · ${formatBpp(job.komla.length, pixels)}` : ''}
                  {' · '}restores {what} byte for byte
                </p>
              )}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
