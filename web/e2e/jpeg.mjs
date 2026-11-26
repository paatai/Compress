// End-to-end check of lossless JPEG recompression against a running dev/preview server:
// compress each JPEG, download the .komla file, restore it, download the JPG and compare it
// with the original byte for byte.
// usage: node e2e/jpeg.mjs <url> <outdir> <images.jpg...>
import { chromium } from 'playwright-core'
import { createHash } from 'node:crypto'
import { mkdirSync, readFileSync } from 'node:fs'
import { basename, join } from 'node:path'

const [url, out, ...images] = process.argv.slice(2)
mkdirSync(out, { recursive: true })
const sha = (p) => createHash('sha256').update(readFileSync(p)).digest('hex')
const browser = await chromium.launch({ executablePath: process.env.CHROME || '/usr/bin/google-chrome-stable', args: ['--no-sandbox'] })
const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, acceptDownloads: true })
const errors = []
page.on('pageerror', (e) => errors.push(String(e)))
page.on('console', (m) => {
  if (m.type() === 'error') errors.push(m.text())
})
await page.goto(url)
let failures = 0
for (const [i, image] of images.entries()) {
  const t0 = Date.now()
  await page.setInputFiles('input[type=file] >> nth=0', image)
  await page.getByText('Bit-exact round trip verified').nth(i).waitFor({ timeout: 600_000 })
  const secs = ((Date.now() - t0) / 1000).toFixed(1)
  const card = page.locator('[data-slot=card]').first()
  const [dl] = await Promise.all([page.waitForEvent('download'), card.getByRole('button', { name: /Download \.komla/ }).click()])
  const komlaPath = join(out, dl.suggestedFilename())
  await dl.saveAs(komlaPath)
  await page.setInputFiles('input[type=file] >> nth=0', komlaPath)
  await page.getByText('Original JPEG restored, checksum verified').first().waitFor({ timeout: 600_000 })
  const card2 = page.locator('[data-slot=card]').first()
  const [dl2] = await Promise.all([page.waitForEvent('download'), card2.getByRole('button', { name: /Download JPG/ }).click()])
  const restored = join(out, `restored-${basename(image)}`)
  await dl2.saveAs(restored)
  const same = sha(restored) === sha(image)
  if (!same) failures++
  const ratio = (100 * (1 - readFileSync(komlaPath).length / readFileSync(image).length)).toFixed(2)
  console.log(`${basename(image)}: ${ratio}% smaller, compress+verify ${secs} s, restored file ${same ? 'identical' : 'DIFFERENT'}`)
}
await page.screenshot({ path: join(out, 'jpeg-jobs.png'), fullPage: true })
console.log('console errors:', errors.length ? errors : 'none')
await browser.close()
if (failures) process.exit(1)
