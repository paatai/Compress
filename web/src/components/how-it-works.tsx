import { Blend, Binary, FileImage, Grid3x3, Layers, Repeat, Sigma } from 'lucide-react'

const steps = [
  {
    icon: Grid3x3,
    title: 'Predict every sample',
    text: 'Each colour value is predicted from pixels already coded: two dozen classic predictors (gradients, MED, GAP and cross-channel ones), a least-squares predictor fitted on a 2-D window around the pixel, and a second least-squares stage that learns how to combine them.',
  },
  {
    icon: Layers,
    title: 'Use the other channels',
    text: 'Green is coded first; red and blue are predicted from their neighbours plus what green just did at the same pixel, which removes most of the colour redundancy.',
  },
  {
    icon: Binary,
    title: 'Code the surprise bit by bit',
    text: 'The prediction error is split into yes/no questions (zero? sign? which magnitude range? which value inside it?). Each answer is coded with a binary arithmetic coder.',
  },
  {
    icon: Blend,
    title: 'Mix many opinions',
    text: 'Dozens of adaptive models estimate each answer: local error statistics, neighbour residuals, and one model per predictor that knows how far that predictor is from the guess and how reliable it has been. Small online logistic mixers and secondary estimators combine them.',
  },
  {
    icon: Repeat,
    title: 'Handle screenshots and graphics',
    text: 'Images with few colours are coded as palette indices. Before coding a pixel, a few flags ask whether it repeats a neighbour or the continuation of a 2-D match found earlier in the image, which captures text, flat areas and repeated UI.',
  },
  {
    icon: FileImage,
    title: 'Recompress JPEGs, byte for byte',
    text: "A JPEG's Huffman-coded data is decoded back to its DCT coefficients. Each block's coefficients are predicted from the neighbouring blocks (least-squares fits learned on the image, smooth boundaries, chroma from luma) and coded with the same context mixing. The decoder regenerates the original Huffman data, headers and metadata exactly.",
  },
  {
    icon: Sigma,
    title: 'Exactly reversible',
    text: 'The decoder runs the very same models, so it reproduces every probability and every pixel. All arithmetic is integer or IEEE-754 basic operations, giving identical results on every CPU and in WebAssembly.',
  },
]

export function HowItWorks() {
  return (
    <section id="how" className="scroll-mt-20 space-y-6">
      <div className="space-y-2">
        <h2 className="text-2xl font-semibold tracking-tight">How it works</h2>
        <p className="max-w-3xl text-muted-foreground">
          Komla is a context-mixing coder in the tradition of PAQ and the leading lossless photo benchmarks. It is slower than PNG, and much smaller.
        </p>
      </div>
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {steps.map((s) => (
          <div key={s.title} className="rounded-xl border bg-card p-5">
            <s.icon className="mb-3 size-5 text-emerald-600" strokeWidth={1.75} />
            <h3 className="mb-1.5 font-medium">{s.title}</h3>
            <p className="text-sm leading-relaxed text-muted-foreground">{s.text}</p>
          </div>
        ))}
      </div>
    </section>
  )
}
