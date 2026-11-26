import { useRef, useState } from 'react'
import { FileArchive, ImageUp } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

export function DropZone({ onFiles }: { onFiles: (files: FileList | File[]) => void }) {
  const [over, setOver] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  return (
    <div
      onDragOver={(e) => {
        e.preventDefault()
        setOver(true)
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault()
        setOver(false)
        if (e.dataTransfer.files.length) onFiles(e.dataTransfer.files)
      }}
      onClick={() => input.current?.click()}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') input.current?.click()
      }}
      className={cn(
        'group relative flex cursor-pointer flex-col items-center justify-center gap-4 rounded-2xl border-2 border-dashed px-6 py-12 text-center transition-colors outline-none sm:py-16',
        'border-border bg-muted/30 hover:border-emerald-500/60 hover:bg-emerald-500/5 focus-visible:border-emerald-500',
        over && 'border-emerald-500 bg-emerald-500/10',
      )}
    >
      <div className="flex items-center gap-3 text-muted-foreground">
        <ImageUp className="size-9 text-emerald-600" strokeWidth={1.5} />
        <span className="text-2xl font-light">→</span>
        <FileArchive className="size-9" strokeWidth={1.5} />
      </div>
      <div className="space-y-1.5">
        <p className="text-lg font-medium">Drop a PNG or BMP to compress it</p>
        <p className="text-sm text-muted-foreground">or drop a .komla file to restore the exact original pixels as PNG</p>
      </div>
      <Button
        type="button"
        variant="outline"
        onClick={(e) => {
          e.stopPropagation()
          input.current?.click()
        }}
      >
        Choose files
      </Button>
      <input
        ref={input}
        type="file"
        multiple
        accept=".png,.bmp,.komla,image/png,image/bmp"
        className="hidden"
        onChange={(e) => {
          if (e.target.files?.length) onFiles(e.target.files)
          e.target.value = ''
        }}
      />
    </div>
  )
}
