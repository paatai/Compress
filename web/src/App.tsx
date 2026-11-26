import { Cpu, Lock, ShieldCheck, Terminal } from 'lucide-react'
import { Benchmark } from '@/components/benchmark'
import { DropZone } from '@/components/drop-zone'
import { HowItWorks } from '@/components/how-it-works'
import { JobCard } from '@/components/job-card'
import { useJobs } from '@/hooks/use-jobs'

function Logo() {
  return (
    <div className="grid size-8 grid-cols-2 gap-0.5 rounded-lg bg-foreground p-1.5" aria-hidden>
      <span className="rounded-[2px] bg-emerald-400" />
      <span className="rounded-[2px] bg-background/80" />
      <span className="rounded-[2px] bg-background/40" />
      <span className="rounded-[2px] bg-emerald-400/70" />
    </div>
  )
}

export default function App() {
  const { jobs, addFiles, compareFormats, checkOriginal, remove } = useJobs()

  return (
    <div
      className="min-h-svh bg-background text-foreground"
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => {
        e.preventDefault()
        if (e.dataTransfer.files.length) addFiles(e.dataTransfer.files)
      }}
    >
      <header className="sticky top-0 z-10 border-b bg-background/80 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-5xl items-center justify-between px-4 sm:px-6">
          <a href="#" className="flex items-center gap-2.5 font-semibold tracking-tight">
            <Logo />
            Komla
          </a>
          <nav className="flex items-center gap-1 text-sm text-muted-foreground sm:gap-4">
            <a className="rounded px-2 py-1 hover:text-foreground" href="#how">
              How it works
            </a>
            <a className="rounded px-2 py-1 hover:text-foreground" href="#benchmark">
              Benchmark
            </a>
            <a className="hidden rounded px-2 py-1 hover:text-foreground sm:inline" href="#cli">
              Command line
            </a>
            <a className="rounded px-2 py-1 hover:text-foreground" href="https://github.com/paatai/Compress">
              Source
            </a>
          </nav>
        </div>
      </header>

      <main className="mx-auto max-w-5xl space-y-16 px-4 py-10 sm:px-6 sm:py-14">
        <section className="space-y-8">
          <div className="max-w-3xl space-y-4">
            <h1 className="text-3xl font-semibold tracking-tight text-balance sm:text-5xl">Lossless image compression, smaller than WebP and JPEG XL.</h1>
            <p className="text-lg text-pretty text-muted-foreground">
              Drop a PNG or BMP and get a <code className="rounded bg-muted px-1 py-0.5 text-base">.komla</code> file that decodes back to exactly the same
              pixels, bit for bit. Everything runs here in your browser; your images never leave your computer.
            </p>
            <div className="flex flex-wrap gap-x-6 gap-y-2 text-sm text-muted-foreground">
              <span className="flex items-center gap-1.5">
                <Lock className="size-4" /> No uploads
              </span>
              <span className="flex items-center gap-1.5">
                <ShieldCheck className="size-4" /> Every file is round-trip verified
              </span>
              <span className="flex items-center gap-1.5">
                <Cpu className="size-4" /> WebAssembly, multi-core for large photos
              </span>
            </div>
          </div>
          <DropZone onFiles={addFiles} />
          {jobs.length > 0 && (
            <div className="space-y-4">
              {jobs.map((j) => (
                <JobCard key={j.id} job={j} onCompare={compareFormats} onCheckOriginal={checkOriginal} onRemove={remove} />
              ))}
            </div>
          )}
          {jobs.length === 0 && (
            <p className="text-center text-sm text-muted-foreground">
              Supports 8- and 16-bit grayscale, RGB and RGBA PNG (including palette PNGs) and uncompressed BMP. Large photos take a while: a 12-megapixel image
              needs roughly half a minute on a recent laptop.
            </p>
          )}
        </section>

        <HowItWorks />
        <Benchmark />

        <section id="cli" className="scroll-mt-20 space-y-4">
          <h2 className="text-2xl font-semibold tracking-tight">Command line</h2>
          <p className="max-w-3xl text-muted-foreground">
            The same codec is available as a command-line tool for macOS and Linux (Rust). Files are interchangeable with this page.
          </p>
          <pre className="overflow-x-auto rounded-xl border bg-muted/40 p-4 text-sm leading-relaxed">
            <code>
              <span className="text-muted-foreground"># build once (needs Rust from rustup.rs)</span>
              {'\n'}git clone https://github.com/paatai/Compress && cd Compress{'\n'}cargo install --path cli{'\n\n'}
              komla compress photo.png{'           '}
              <span className="text-muted-foreground"># writes photo.png.komla</span>
              {'\n'}komla decompress photo.png.komla{'   '}
              <span className="text-muted-foreground"># restores photo.png, checks the CRC</span>
              {'\n'}komla info photo.png.komla
            </code>
          </pre>
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Terminal className="size-4" /> See the README for build instructions, the full benchmark and how to reproduce it.
          </p>
        </section>
      </main>

      <footer className="border-t">
        <div className="mx-auto flex max-w-5xl flex-col gap-2 px-4 py-6 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between sm:px-6">
          <span>Komla 1.0 · lossless image compression by context mixing</span>
          <span>Runs locally in WebAssembly. Nothing is uploaded or stored.</span>
        </div>
      </footer>
    </div>
  )
}
