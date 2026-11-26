import { chromium } from 'playwright-core'
const browser = await chromium.launch({ executablePath: '/usr/bin/google-chrome-stable', args: ['--no-sandbox'] })
const page = await browser.newPage({ viewport: { width: 1100, height: 800 } })
const errors = []
page.on('pageerror', (e) => errors.push(String(e)))
await page.goto('http://127.0.0.1:47124/')
await page.setInputFiles('input[type=file] >> nth=0', ['/tmp/fake.png', '/tmp/truncated.kolmo'])
await page.waitForTimeout(8000)
const alerts = await page.locator('.text-destructive').allInnerTexts()
console.log('error messages shown:', JSON.stringify(alerts))
await page.screenshot({ path: '/tmp/shot-errors.png' })
console.log('page errors:', errors.length ? errors : 'none')
await browser.close()
