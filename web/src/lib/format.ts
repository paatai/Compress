export function formatBytes(n: number): string {
  if (n < 1000) return `${n} B`
  if (n < 1e6) return `${(n / 1e3).toFixed(n < 1e4 ? 2 : 1)} KB`
  if (n < 1e9) return `${(n / 1e6).toFixed(2)} MB`
  return `${(n / 1e9).toFixed(2)} GB`
}

export function formatBpp(bytes: number, pixels: number): string {
  return `${((bytes * 8) / pixels).toFixed(3)} bpp`
}

export function formatSeconds(s: number): string {
  return s < 10 ? `${s.toFixed(2)} s` : `${s.toFixed(1)} s`
}

export function describeChannels(channels: number, depth: number): string {
  const kind = ['', 'Grayscale', 'Grayscale + alpha', 'RGB', 'RGBA'][channels] ?? `${channels} channels`
  return `${kind}, ${depth}-bit`
}

export function download(data: Uint8Array, name: string, type: string) {
  const url = URL.createObjectURL(new Blob([data as BlobPart], { type }))
  const a = document.createElement('a')
  a.href = url
  a.download = name
  document.body.appendChild(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

export function stripExt(name: string): string {
  return name.replace(/\.(png|bmp|komla)$/i, '')
}
