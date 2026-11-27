// Cross-target determinism check: the WebAssembly build must produce byte-identical
// .komla files to the native CLI and decode native files exactly. PNG/BMP inputs are compared
// by pixels, JPEG inputs (default and fast mode) by the restored file bytes.
// usage: node tests/cross_target.mjs <komla.wasm> <native komla binary> <images...>
import { readFileSync, mkdtempSync, rmSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const [wasmPath, nativeBin, ...images] = process.argv.slice(2)
const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {
  env: { komla_progress: () => {} },
})
const k = instance.exports

function call(fn, bytes) {
  const ptr = k.komla_alloc(bytes.length)
  new Uint8Array(k.memory.buffer, ptr, bytes.length).set(bytes)
  const rc = k[fn](ptr, bytes.length)
  k.komla_free(ptr, bytes.length)
  if (rc !== 0) {
    const e = new Uint8Array(k.memory.buffer, k.komla_error_ptr(), k.komla_error_len())
    throw new Error(new TextDecoder().decode(e))
  }
  return new Uint8Array(k.memory.buffer, k.komla_result_ptr(), k.komla_result_len()).slice()
}

const isJpeg = (b) => b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff
const tmp = mkdtempSync(join(tmpdir(), 'komla-xt-'))
let failures = 0
for (const img of images) {
  const src = readFileSync(img)
  const jpeg = isJpeg(src)
  const t0 = performance.now()
  const wasmOut = call(jpeg ? 'komla_jpeg_compress' : 'komla_compress', src)
  const encMs = performance.now() - t0
  const nativePath = join(tmp, 'n.komla')
  execFileSync(nativeBin, ['compress', img, '-o', nativePath], { stdio: 'ignore' })
  const nativeOut = readFileSync(nativePath)
  const same = Buffer.compare(Buffer.from(wasmOut), nativeOut) === 0
  const t1 = performance.now()
  let ok
  if (jpeg) {
    const restored = call('komla_jpeg_decompress', nativeOut)
    ok = Buffer.compare(Buffer.from(restored), src) === 0
  } else {
    const decoded = call('komla_decode_pixels', nativeOut)
    const original = call('komla_decode_pixels', src)
    ok = Buffer.compare(Buffer.from(decoded), Buffer.from(original)) === 0
  }
  const decMs = performance.now() - t1
  if (!same || !ok) failures++
  const what = jpeg ? `${(100 * (1 - wasmOut.length / src.length)).toFixed(2)}% smaller, restored file` : 'pixels'
  console.log(`${img.split('/').pop()}: ${wasmOut.length} bytes, wasm==native ${same}, ${what} ${ok ? 'OK' : 'MISMATCH'}, wasm enc ${(encMs / 1000).toFixed(2)}s dec ${(decMs / 1000).toFixed(2)}s`)
  if (jpeg) {
    // fast mode: the result starts with an info block (see jpeg_info_block in wasm/src/lib.rs)
    const t2 = performance.now()
    const r = call('komla_jpeg_compress_fast', src)
    const v = new DataView(r.buffer)
    let o = 16
    for (let i = 0; i < 2; i++) o += 4 + v.getUint32(o, true)
    const fastWasm = r.slice(o)
    const fencMs = performance.now() - t2
    const fastPath = join(tmp, 'f.komla')
    execFileSync(nativeBin, ['compress', '--fast', img, '-o', fastPath], { stdio: 'ignore' })
    const fastNative = readFileSync(fastPath)
    const fsame = Buffer.compare(Buffer.from(fastWasm), fastNative) === 0
    const t3 = performance.now()
    const fok = Buffer.compare(Buffer.from(call('komla_jpeg_decompress', fastNative)), src) === 0
    const fdecMs = performance.now() - t3
    if (!fsame || !fok) failures++
    console.log(`  fast mode: ${fastWasm.length} bytes, wasm==native ${fsame}, restored file ${fok ? 'OK' : 'MISMATCH'}, wasm enc ${(fencMs / 1000).toFixed(2)}s dec ${(fdecMs / 1000).toFixed(2)}s`)
  }
}
rmSync(tmp, { recursive: true })
if (failures) {
  console.error(`${failures} failures`)
  process.exit(1)
}
