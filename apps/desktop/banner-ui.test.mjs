// The desktop WebView's actual page, without opening the installed app or taking over the Mac.
// Install web's development dependencies and Chromium, then: node --test apps/desktop/banner-ui.test.mjs
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import test from 'node:test';
import assert from 'node:assert/strict';

const require = createRequire(new URL('../../web/package.json', import.meta.url));
const { chromium } = require('@playwright/test');
const html = await readFile(new URL('../../crates/extend-agent/src/ui/page.html', import.meta.url), 'utf8');
const session = { silicon_id: 'si:chef', session_id: 'a3f', since: new Date().toISOString() };
const base = {
  phase: 'online', app_version: '1.1.0', computer_word: 'Mac', in_use_indicator: 'shown',
  pairs: [{ device_id: '7c1e09ab', name: 'Mac', owner: 'c:alice', team: 'acme', phase: 'online' }],
  attached: [], setup: { state: 'complete', steps: [] }, capabilities: [], wake_requests: [],
};

async function open(view) {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 760, height: 1000 } });
  await page.evaluate(view => {
    window.__view = view;
    window.sent = [];
    window.ipc = { postMessage: body => window.sent.push(JSON.parse(body)) };
  }, view);
  await page.setContent(html);
  return { browser, page };
}

test('carried switches target their own device, remain usable offline, and retain Stop', async () => {
  const { browser, page } = await open('main');
  try {
    const name = 'iPad "study" <safe>';
    const state = { ...base, phase: 'reconnecting', indicator_sync_pending: true,
      in_use: session, in_use_indicator: 'hidden', attached: [{
        device_id: 'aabbccdd', name, os: 'ipados', online: false, in_use_indicator: 'shown', in_use: session,
      }] };
    await page.evaluate(s => window.__extend(s, false), state);
    assert.equal(await page.locator('#banner-sync').isVisible(), true);
    await page.getByRole('button', { name: `Turn off in-use banner for ${name}`, exact: true }).click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'set_banner', target: 'aabbccdd', on: false });
    await page.locator('#banner-toggle').click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'set_banner', on: true });
    await page.getByRole('button', { name: `Stop si:chef on ${name}`, exact: true }).click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'stop', target: 'aabbccdd' });
    assert.equal(await page.locator('#in-use [data-action="stop"]').isVisible(), true);
    assert.equal(await page.locator('#attached-list .row-title').textContent(), `${name}iPadOS`);
  } finally { await browser.close(); }
});

test('banner drag and collapse emit separate actions; collapsed Stop and takeover Done stay accessible', async () => {
  const { browser, page } = await open('banner');
  try {
    const state = { ...base, in_use: session, banner_targets: [null], environment: { name: 'Banner test', state: 'ready' } };
    await page.evaluate(s => window.__extend(s, false), state);
    await page.locator('[data-action="banner_minimize"]').click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'banner_minimize' });
    assert.equal(await page.evaluate(() => window.sent.filter(x => x.action === 'banner_drag').length), 0);
    await page.locator('.banner-drag').dispatchEvent('mousedown', { button: 0 });
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'banner_drag' });
    await page.evaluate(s => window.__extend(s, true), state);
    assert.equal(await page.locator('#banner-env').isVisible(), true);
    assert.equal(await page.locator('#banner-text').isVisible(), false);
    await page.locator('#banner-button').click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'stop' });
    await page.locator('#banner-restore').click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'banner_restore' });
    const takeover = { ...state, in_use_indicator: 'hidden', takeover: { session_id: 'a3f', reason: 'Sign in', expires_at: new Date(Date.now() + 60000).toISOString() } };
    await page.evaluate(s => window.__extend(s, true), takeover);
    await page.locator('#banner-button').click();
    assert.deepEqual(await page.evaluate(() => window.sent.at(-1)), { action: 'takeover_done' });
    assert.equal(await page.locator('#banner-stop').isVisible(), true);
  } finally { await browser.close(); }
});
