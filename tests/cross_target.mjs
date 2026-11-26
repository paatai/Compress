// Cross-target determinism check: the WebAssembly build must produce byte-identical
// .kolmo files to the native CLI and decode native files exactly.
// usage: node tests/cross_target.mjs <kolmo.wasm> <native kolmo binary> <images...>
import { readFileSync, mkdtempSync, rmSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const [wasmPath, nativeBin, ...images] = process.argv.slice(2)
const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {
  env: { kolmo_progress: () => {} },
})
const k = instance.exports

function call(fn, bytes) {
  const ptr = k.kolmo_alloc(bytes.length)
  new Uint8Array(k.memory.buffer, ptr, bytes.length).set(bytes)
  const rc = k[fn](ptr, bytes.length)
  k.kolmo_free(ptr, bytes.length)
  if (rc !== 0) {
    const e = new Uint8Array(k.memory.buffer, k.kolmo_error_ptr(), k.kolmo_error_len())
    throw new Error(new TextDecoder().decode(e))
  }
  return new Uint8Array(k.memory.buffer, k.kolmo_result_ptr(), k.kolmo_result_len()).slice()
}

const tmp = mkdtempSync(join(tmpdir(), 'kolmo-xt-'))
let failures = 0
for (const img of images) {
  const src = readFileSync(img)
  const t0 = performance.now()
  const wasmOut = call('kolmo_compress', src)
  const encMs = performance.now() - t0
  const nativePath = join(tmp, 'n.kolmo')
  execFileSync(nativeBin, ['compress', img, '-o', nativePath], { stdio: 'ignore' })
  const nativeOut = readFileSync(nativePath)
  const same = Buffer.compare(Buffer.from(wasmOut), nativeOut) === 0
  const t1 = performance.now()
  const decoded = call('kolmo_decode_pixels', nativeOut)
  const decMs = performance.now() - t1
  const original = call('kolmo_decode_pixels', src)
  const pixelsOk = Buffer.compare(Buffer.from(decoded), Buffer.from(original)) === 0
  if (!same || !pixelsOk) failures++
  console.log(`${img.split('/').pop()}: ${wasmOut.length} bytes, wasm==native ${same}, pixels ${pixelsOk ? 'OK' : 'MISMATCH'}, wasm enc ${(encMs / 1000).toFixed(2)}s dec ${(decMs / 1000).toFixed(2)}s`)
}
rmSync(tmp, { recursive: true })
if (failures) {
  console.error(`${failures} failures`)
  process.exit(1)
}
