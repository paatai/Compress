// Cross-target determinism check: the WebAssembly build must produce byte-identical
// .komla files to the native CLI and decode native files exactly. PNG/BMP inputs are compared
// by pixels, JPEG inputs (default, fast and strong mode) by the restored file bytes.
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
  // fast and strong modes: the result starts with an info block (see jpeg_info_block in
  // wasm/src/lib.rs)
  for (const [mode, fn] of jpeg ? [['fast', 'komla_jpeg_compress_fast'], ['strong', 'komla_jpeg_compress_strong']] : []) {
    const t2 = performance.now()
    const r = call(fn, src)
    const v = new DataView(r.buffer)
    let o = 16
    for (let i = 0; i < 2; i++) o += 4 + v.getUint32(o, true)
    const modeWasm = r.slice(o)
    const mencMs = performance.now() - t2
    const modePath = join(tmp, 'm.komla')
    execFileSync(nativeBin, ['compress', `--${mode}`, img, '-o', modePath], { stdio: 'ignore' })
    const modeNative = readFileSync(modePath)
    const msame = Buffer.compare(Buffer.from(modeWasm), modeNative) === 0
    const t3 = performance.now()
    const mok = Buffer.compare(Buffer.from(call('komla_jpeg_decompress', modeNative)), src) === 0
    const mdecMs = performance.now() - t3
    if (!msame || !mok) failures++
    console.log(`  ${mode} mode: ${modeWasm.length} bytes, wasm==native ${msame}, restored file ${mok ? 'OK' : 'MISMATCH'}, wasm enc ${(mencMs / 1000).toFixed(2)}s dec ${(mdecMs / 1000).toFixed(2)}s`)
  }
}
rmSync(tmp, { recursive: true })
if (failures) {
  console.error(`${failures} failures`)
  process.exit(1)
}
