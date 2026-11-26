// End-to-end smoke test against a running dev/preview server.
// usage: node e2e/smoke.mjs <url> <image.png> <outdir>
import { chromium } from 'playwright-core'
import { mkdirSync } from 'node:fs'
import { join } from 'node:path'

const [url, image, out] = process.argv.slice(2)
mkdirSync(out, { recursive: true })
const browser = await chromium.launch({ executablePath: process.env.CHROME || '/usr/bin/google-chrome-stable', args: ['--no-sandbox'] })
const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, acceptDownloads: true })
const errors = []
page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()) })
page.on('pageerror', (e) => errors.push(String(e)))
await page.goto(url)
await page.screenshot({ path: join(out, '1-empty.png'), fullPage: true })
const t0 = Date.now()
await page.setInputFiles('input[type=file] >> nth=0', image)
await page.getByText('Bit-exact round trip verified').waitFor({ timeout: 300_000 })
console.log(`compress + verify: ${((Date.now() - t0) / 1000).toFixed(1)} s`)
await page.screenshot({ path: join(out, '2-compressed.png'), fullPage: true })
const [dl] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: /Download \.kolmo/ }).click()])
const kolmoPath = join(out, await dl.suggestedFilename())
await dl.saveAs(kolmoPath)
await page.getByRole('button', { name: /Compare with WebP and JPEG XL/ }).click()
await page.getByText(/lossless, effort/).waitFor({ timeout: 600_000 })
await page.screenshot({ path: join(out, '3-compared.png'), fullPage: true })
await page.setInputFiles('input[type=file] >> nth=0', kolmoPath)
await page.getByText('Decoded and checksum verified').waitFor({ timeout: 300_000 })
await page.screenshot({ path: join(out, '4-restored.png'), fullPage: true })
const [dl2] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: /Download PNG/ }).click()])
await dl2.saveAs(join(out, 'restored.png'))
await page.setViewportSize({ width: 390, height: 844 })
await page.screenshot({ path: join(out, '5-mobile.png'), fullPage: true })
console.log('console errors:', errors.length ? errors : 'none')
await browser.close()
