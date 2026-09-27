#!/usr/bin/env node
/** Actual tagged 1.0 website → current website, same browser origin/storage, real isolated
 * current service. Synthetic local IAM/Briefcase/Ting and WebSocket device only.
 * Run: node e2e/web-upgrade-rehearsal.mjs [--out target/web-upgrade-rehearsal]
 * Requires built target/debug/extend-service, web/node_modules + Playwright Chromium,
 * and silicon-extend-postgres. Never reuses a service or database; only owned PIDs/DB cleaned.
 */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { dirname, join, resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const web = join(root, 'web');
const require = createRequire(join(web, 'package.json'));
const { chromium, expect } = require('@playwright/test');
const WebSocket = require('ws');
const args = process.argv.slice(2);
if (args.length && !(args.length === 2 && args[0] === '--out')) throw Error('Usage: node e2e/web-upgrade-rehearsal.mjs [--out <new directory>]');
const out = resolve(root, args[1] || `target/web-upgrade-rehearsal-${Date.now()}`);
if (fs.existsSync(out)) throw Error(`Refusing existing output directory: ${out}`);
const binary = join(root, 'target/debug/extend-service');
assert(fs.existsSync(binary), 'Build the current debug service first');
const cleanEnv = { PATH: process.env.PATH, HOME: process.env.HOME, TMPDIR: process.env.TMPDIR || '/tmp' };
const db = `extend_web_upgrade_${process.pid}_${Date.now()}`;
const docker = ['exec', 'silicon-extend-postgres'];
const children = new Set(), devices = [];
const results = [], errors = [], outside = [], apiResponses = [];
let databaseCreated = false, service, preview, browser, page, api, site;
fs.mkdirSync(out, { recursive: true });
const log = text => { console.log(text); fs.appendFileSync(join(out, 'run.log'), `${text}\n`); };
const pass = (name, details = {}) => { results.push({ name, ...details }); log(`PASS ${name}`); };
const sleep = ms => new Promise(r => setTimeout(r, ms));
function run(command, argv, options = {}) {
  const p = spawnSync(command, argv, { cwd: root, env: cleanEnv, encoding: 'utf8', maxBuffer: 1 << 28, ...options });
  if (p.status !== 0) throw Error(`${command} ${argv.join(' ')} failed (${p.status}): ${p.stderr || p.stdout}`);
  return p.stdout;
}
function start(command, argv, name, options = {}) {
  const fd = fs.openSync(join(out, `${name}.log`), 'a');
  const child = spawn(command, argv, { cwd: root, env: cleanEnv, stdio: ['ignore', fd, fd], ...options });
  fs.closeSync(fd); children.add(child); child.once('exit', () => children.delete(child)); return child;
}
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  await Promise.race([once(child, 'exit'), sleep(3000)]);
  if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await once(child, 'exit'); }
}
async function port() {
  const server = createServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const number = server.address().port; await new Promise(r => server.close(r)); return number;
}
async function ready(url, child) {
  for (let n = 0; n < 150; n++) {
    if (child.exitCode !== null) throw Error(`Child exited before ${url}: ${child.exitCode}`);
    try { if ((await fetch(url)).ok) return; } catch {}
    await sleep(200);
  }
  throw Error(`Not ready: ${url}`);
}
async function call(method, path, data, token, team = 'acme', type) {
  const response = await fetch(api + path, { method, headers: {
    'Content-Type': 'application/json', 'Idempotency-Key': randomUUID(),
    ...(token ? { Authorization: `Bearer ${token}`, 'X-Org-ID': team } : {}),
  }, body: data === undefined ? undefined : JSON.stringify({ type: type || 'request', data }) });
  const raw = await response.text(), value = raw ? JSON.parse(raw) : null;
  if (!response.ok) throw Error(`${method} ${path}: ${response.status} ${raw}`);
  return value?.data;
}
const login = id => call('POST', '/api/v1/auth/login', { slt: id }, null, 'acme', 'login').then(r => r.access_token);
async function device(token, team, name) {
  const e = await call('POST', '/api/v1/enrollments', { os: 'android', os_version: '15', model: 'Synthetic website fixture', app_version: '1.1.0' }, null, team, 'enrollment');
  const d = await call('POST', '/api/v1/pairings', { pairing_code: e.pairing_code, name }, token, team, 'pairing');
  const r = await fetch(`${api}/api/v1/enrollments/${e.enrollment_id}`, { headers: { Authorization: `Extend-Enrollment ${e.enrollment_secret}` } });
  const credential = (await r.json()).data.device_credential;
  const ws = new WebSocket(`${api.replace(/^http/, 'ws')}/api/v1/device/connect`, { headers: { Authorization: `Extend-Device ${credential}` } });
  devices.push(ws); ws.on('message', raw => { const f = JSON.parse(raw); if (f.type === 'ping') ws.send(JSON.stringify({ type: 'pong', nonce: f.nonce })); });
  await once(ws, 'open');
  ws.send(JSON.stringify({ type: 'hello', app_version: '1.1.0', os: 'android', os_version: '15', model: 'Synthetic website fixture', features: ['setup_retry'], engine_version: null,
    capabilities: ['screen.read','screen.capture','input.touch','input.text','nav.system','apps.launch','apps.list','takeover','notifications','links'], missing: [],
    setup: { state: 'complete', steps: [{ key: 'accessibility', title: 'Accessibility enabled', status: 'done' }] } }));
  ws.send(JSON.stringify({ type: 'awake', awake: false, sleep_state: 'screen_off', input_seen: false, run: randomUUID(), seq: 1 }));
  await expect.poll(async () => (await call('GET', `/api/v1/devices/${d.device_id}`, undefined, token, team)).online).toBe(true);
  return d;
}
async function buildSources() {
  const old = join(out, 'legacy'), current = join(out, 'current');
  fs.mkdirSync(old); fs.mkdirSync(current);
  const tar = run('git', ['archive', '--format=tar', 'v1.0.0', 'web'], { encoding: null });
  run('tar', ['-x', '-C', old], { input: tar });
  fs.cpSync(web, join(current, 'web'), { recursive: true, filter: source => !['node_modules','dist','test-results','.vite','.cache'].includes(basename(source)) && !basename(source).startsWith('.env') });
  for (const [name, dir] of [['legacy',old],['current',current]]) {
    const cwd = join(dir, 'web'); fs.symlinkSync(join(web, 'node_modules'), join(cwd, 'node_modules'), 'dir');
    const child = start(join(web, 'node_modules/.bin/vite'), ['build','--logLevel','warn'], `${name}-build`, { cwd, env: { ...cleanEnv, VITE_EXTEND_API_URL: 'same-origin', VITE_IAM_LOGIN_URL: '' } });
    const [code] = await once(child, 'exit'); assert.equal(code, 0, `${name} website build`);
  }
  const metadata = { legacy_tag: 'v1.0.0', legacy_commit: run('git',['rev-parse','v1.0.0']).trim(), current_commit: run('git',['rev-parse','HEAD']).trim(),
    service_sha256: createHash('sha256').update(fs.readFileSync(binary)).digest('hex'), current_web_worktree_diff: run('git',['diff','--','web']),
    versions: [JSON.parse(fs.readFileSync(join(old,'web/package.json'))).version, JSON.parse(fs.readFileSync(join(current,'web/package.json'))).version] };
  fs.writeFileSync(join(out,'source.json'),JSON.stringify(metadata,null,2));
  pass('Actual tagged v1.0.0 and current source bundles built', metadata.versions);
  return { old: join(old,'web'), current: join(current,'web') };
}
async function serve(cwd, webPort, name) {
  preview = start(join(web, 'node_modules/.bin/vite'), ['preview','--host','127.0.0.1','--port',String(webPort),'--strictPort'], name,
    { cwd, env: { ...cleanEnv, EXTEND_API_PROXY: api } });
  await ready(site,preview);
}
async function shot(name) { await page.screenshot({ path: join(out,`${name}.png`), fullPage: true }); }
async function grantAndSession(label, d, alice, chef) {
  await page.goto(`${site}/devices/${d.device_id}`);
  await expect(page.getByTestId('device-name')).toHaveText(d.name);
  await page.getByText('Give another Silicon access').click();
  await page.getByTestId('grant-input').fill('si:sous');
  await page.getByTestId('grant-submit').click();
  await expect(page.locator('[data-testid="grant"][data-silicon="si:sous"]')).toBeVisible();
  const grants = await call('GET', `/api/v1/devices/${d.device_id}/access`, undefined, alice);
  assert(grants.items.some(g => g.silicon_id === 'si:sous' && g.team === 'acme'));
  pass(`${label}: real grant from website belongs to acme`);
  const session = await call('POST','/api/v1/sessions',{device_id:d.device_id},chef,'acme','session');
  await expect(page.getByTestId('in-use-silicon')).toHaveText('si:chef', { timeout: 12000 });
  await expect(page.getByTestId('in-use-card')).toContainText(session.session_id);
  await shot(`${label}-session`);
  await call('POST', `/api/v1/sessions/${session.session_id}/takeover`, {reason:'Synthetic upgrade check'},chef,'acme','takeover');
  await expect(page.getByTestId('takeover-reason')).toHaveText('Synthetic upgrade check', {timeout:12000});
  await page.getByTestId('takeover-done').click();
  await expect(page.getByTestId('takeover')).toHaveCount(0, {timeout:12000});
  await page.getByTestId('stop-session').click();
  await expect(page.getByTestId('in-use-card')).toContainText('No Silicon is using', {timeout:12000});
  assert.equal((await call('GET',`/api/v1/sessions/${session.session_id}`,undefined,chef)).state,'ended');
  pass(`${label}: session, takeover Done and Stop through real service`);
  await page.locator('[data-testid="grant"][data-silicon="si:sous"]').getByTestId('revoke').click();
  await expect(page.locator('[data-testid="grant"][data-silicon="si:sous"]')).toHaveCount(0);
  pass(`${label}: grant revoke succeeds`);
}
const started = Date.now();
try {
  const sources = await buildSources();
  const apiPort = await port(), webPort = await port(); api = `http://127.0.0.1:${apiPort}`; site = `http://127.0.0.1:${webPort}`;
  run('docker',[...docker,'createdb','-U','extend',db]); databaseCreated=true;
  service = start(binary, [], 'service', { env: { ...cleanEnv, EXTEND_ENVIRONMENT:'development', EXTEND_DATABASE_URL:`postgres://extend:extend@127.0.0.1:5440/${db}`,
    EXTEND_BIND:`127.0.0.1:${apiPort}`, EXTEND_PUBLIC_URL:api, EXTEND_WEBSITE_URL:site, EXTEND_DATA_DIR:join(out,'data'),
    EXTEND_IAM_MODE:'local', EXTEND_FILES_MODE:'local', EXTEND_TING_MODE:'local', EXTEND_IAM_PUBLIC_URL:`${api}/dev/iam`, EXTEND_IAM_LOGIN_URL:`${api}/dev/iam/login`,
    EXTEND_LOCAL_MEMBERS:'c:alice@acme+labs,c:bob@acme,si:chef@acme,si:sous@acme,si:scout@labs' } });
  await ready(`${api}/ready`, service);
  const alice=await login('c:alice'), bob=await login('c:bob'), chef=await login('si:chef');
  const main=await device(alice,'acme','Compatibility Pixel'), lab=await device(alice,'labs','Compatibility Lab'), hidden=await device(bob,'acme','Other Carbon private device');
  await call('PUT',`/api/v1/devices/${main.device_id}/access/si:chef`,{},alice,'acme','access');
  await call('PATCH',`/api/v1/devices/${main.device_id}`,{in_use_indicator:'hidden'},alice,'acme','device');
  await call('POST',`/api/v1/devices/${main.device_id}/wake-requests`,{reason:'Synthetic compatibility wake'},chef,'acme','wake_request');
  const view=await call('GET',`/api/v1/devices/${main.device_id}`,undefined,alice);
  assert.equal(view.in_use_indicator,'hidden'); assert.equal(view.awake,false); assert.equal(view.open_wake_requests,1);
  fs.writeFileSync(join(out,'additive-device-response.json'),JSON.stringify(view,null,2));
  pass('Real service fixture carries awake, wake_requests and in_use_indicator additions');
  await serve(sources.old,webPort,'legacy-preview');
  browser=await chromium.launch({headless:true});const context=await browser.newContext({viewport:{width:1280,height:900}});page=await context.newPage();
  page.on('pageerror',e=>errors.push(e.message));
  await context.route('**/*',route=>{const url=new URL(route.request().url());if(['http:','https:'].includes(url.protocol)&& ![site,api].includes(url.origin)){outside.push(url.href);return route.abort();} return route.continue();});
  page.on('response',res=>{if(res.url().includes('/api/'))apiResponses.push({url:new URL(res.url()).pathname,status:res.status()});});
  await page.goto(site);await page.getByTestId('slt-input').fill('c:alice');await page.getByTestId('slt-submit').click();
  await expect(page.getByTestId('devices-page')).toBeVisible();await expect(page.getByTestId('member-id')).toHaveText('c:alice');
  await expect(page.getByTestId('device-list').getByTestId('device-row')).toHaveCount(2);await expect(page.getByTestId('devices-page')).not.toContainText(hidden.name);
  await page.getByTestId('team-picker').selectOption('labs');await expect(page.getByTestId('device-list').getByTestId('device-row')).toHaveCount(2);
  await page.getByTestId('tab-team').click();await expect(page.getByText('No other Carbon in this team has made a device visible.')).toBeVisible();
  await page.getByTestId('team-picker').selectOption('acme');await page.getByTestId('tab-mine').click();await shot('legacy-list');
  pass('Legacy login and cross-Team own-device lists; other Carbon hidden; Team tab empty');
  await page.goto(`${site}/devices/${main.device_id}`);await expect(page.getByTestId('device-name')).toHaveText(main.name);
  await page.getByTestId('visibility-team').check();await expect(page.getByTestId('visibility-personal')).toBeChecked();
  pass('Legacy detail decodes additive fields and deprecated visibility remains personal');
  await grantAndSession('legacy',main,alice,chef);
  const authBefore=await page.evaluate(()=>localStorage.getItem('extend.auth.production'));assert(authBefore);
  await shot('legacy-detail');await page.goto('about:blank');await stop(preview);
  await serve(sources.current,webPort,'current-preview');await page.goto(`${site}/devices`);
  await expect(page.getByTestId('devices-page')).toBeVisible();await expect(page.getByTestId('member-id')).toHaveText('c:alice');
  assert.equal(await page.evaluate(()=>localStorage.getItem('extend.auth.production')),authBefore);
  await expect(page.getByTestId('device-list').getByTestId('device-row')).toHaveCount(2);await expect(page.getByTestId('tab-team')).toHaveCount(0);
  pass('Current website upgrade retains legacy browser login and device list on same origin');await shot('current-list');
  await grantAndSession('current',main,alice,chef);
  await expect(page.getByTestId('banner-setting')).toHaveAttribute('data-indicator','hidden');await page.getByTestId('banner-toggle').click();
  await expect(page.getByTestId('banner-setting')).toHaveAttribute('data-indicator','shown');
  assert.equal((await call('GET',`/api/v1/devices/${main.device_id}`,undefined,alice)).in_use_indicator,'shown');
  await shot('current-detail');pass('Current website renders and changes shared banner setting');
  assert.deepEqual(errors,[]);assert.deepEqual(outside,[]);
  pass('No browser page errors or external network requests');
} catch(error) {
  errors.push(String(error.stack||error));log(`FAIL ${error.stack||error}`);
  if(page)await shot('failure').catch(()=>{});
  process.exitCode=1;
} finally {
  await browser?.close().catch(()=>{});
  for(const ws of devices)ws.terminate();
  for(const child of [...children])await stop(child);
  let cleanupError=null;
  try {if(databaseCreated)run('docker',[...docker,'dropdb','-U','extend',db]);}catch(e){cleanupError=String(e);process.exitCode=1;}
  const report={passed:results.length,results,errors,outside,apiResponses,seconds:(Date.now()-started)/1000,api,site,database:db,database_removed:databaseCreated&&!cleanupError,cleanupError,
    limits:['Actual v1.0.0 tag source bundle and current source bundle; current Rust service/SQL','Synthetic local IAM, file and Ting providers; fake WebSocket device','Not real IAM consent, native app execution, physical devices, public deployment or CDN caching']};
  fs.writeFileSync(join(out,'report.json'),JSON.stringify(report,null,2));log(`Finished: ${results.length} passed; output ${out}; owned database removed: ${report.database_removed}`);
}
