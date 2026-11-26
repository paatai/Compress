// Builds the Rust codec to WebAssembly and copies it into src/wasm/ for Vite.
import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync, statSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
execFileSync('cargo', ['build', '-p', 'komla-wasm', '--target', 'wasm32-unknown-unknown', '--profile', 'wasm'], {
  cwd: root,
  stdio: 'inherit',
  env: { ...process.env, RUSTFLAGS: '-C target-feature=+simd128' },
})
const out = join(root, 'web', 'src', 'wasm', 'komla.wasm')
mkdirSync(dirname(out), { recursive: true })
copyFileSync(join(root, 'target', 'wasm32-unknown-unknown', 'wasm', 'komla_wasm.wasm'), out)
console.log(`komla.wasm: ${(statSync(out).size / 1024).toFixed(0)} KiB`)
