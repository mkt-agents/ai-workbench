// Run: node scripts/fixtures/test-model-budget-shortcut.mjs
// Requires Playwright (or PLAYWRIGHT_MODULE=/absolute/path/to/playwright) and installed Edge.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { createServer } from 'vite';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const root = resolve(import.meta.dirname, '../..');
const scratch = resolve(root, '.build-tmp', `model-budget-shortcut-${randomUUID()}`);
await mkdir(scratch, { recursive: true });
const server = await createServer({ root, server: { host: '127.0.0.1', port: 0, strictPort: false }, logLevel: 'error' });
let browser;
const errors = [];
let assertions = 0;
const check = (value, message) => { assertions++; assert(value, message); };
try {
  await server.listen();
  const port = server.httpServer.address().port;
  const url = `http://127.0.0.1:${port}/scripts/fixtures/model-budget-shortcut.html`;
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  await page.goto(url);
  await page.waitForFunction(() => Boolean(window.modelBudgetShortcutFixture));
  check((await page.title()) === 'Model budget shortcut regression', 'fixture page identity');
  check(await page.locator('vite-error-overlay').count() === 0, 'no framework error overlay');

  for (const scenario of [
    { locale: 'en-US', before: 48000, after: 24000, shortcut: 'Control+Enter' },
    { locale: 'zh-CN', before: 24000, after: 96000, shortcut: 'Meta+Enter' },
  ]) {
    await page.evaluate(({ locale, before }) => window.modelBudgetShortcutFixture.reset(locale, before), scenario);
    const labels = await page.evaluate(() => window.modelBudgetShortcutFixture.labels());
    await page.locator('.ai-model-actions-tools button[aria-haspopup="menu"]').click();
    await page.getByRole('menuitem', { name: labels.edit, exact: true }).click();
    const modal = page.locator('.models-modal');
    const input = modal.getByRole('spinbutton', { name: labels.maxTokens, exact: true });
    await input.waitFor();
    check(await input.inputValue() === '12345', 'real modal retains exact manual output');
    await page.waitForFunction(cap => window.modelBudgetShortcutFixture.snapshot().calls.some(call =>
      call.command === 'resolve_model_budget' && call.args.contextTokens === cap), scenario.before);
    await page.waitForFunction(() => document.querySelector('.ai-budget-preview')?.getAttribute('aria-busy') === 'false');

    // Only global settings change while the same form stays mounted. This catches
    // stale shortcut closures even though the preview effect has already rerendered.
    const originalForm = await modal.locator('input').evaluateAll(inputs => inputs.map(node => node.value));
    await page.evaluate(cap => window.modelBudgetShortcutFixture.setContextLimit(cap), scenario.after);
    await page.waitForFunction(cap => window.modelBudgetShortcutFixture.snapshot().calls.some(call =>
      call.command === 'resolve_model_budget' && call.args.contextTokens === cap), scenario.after);
    await page.waitForFunction(() => document.querySelector('.ai-budget-preview')?.getAttribute('aria-busy') === 'false');
    const capRow = modal.locator('.ai-budget-values > div').filter({ has: page.getByText(labels.configuredCap, { exact: true }) });
    check((await capRow.textContent()).includes(String(scenario.after)), 'preview renders the updated global limit');
    check(JSON.stringify(await modal.locator('input').evaluateAll(inputs => inputs.map(node => node.value))) === JSON.stringify(originalForm), 'global budget change does not mutate or remount the draft');
    const mark = await page.evaluate(() => window.modelBudgetShortcutFixture.snapshot().calls.length);
    await input.focus();
    for (const signal of ['isComposing', 'keyCode']) {
      await input.evaluate((node, signal) => {
        const event = new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true, isComposing: signal === 'isComposing' });
        if (signal === 'keyCode') Object.defineProperty(event, 'keyCode', { value: 229 });
        node.dispatchEvent(event);
      }, signal);
      await page.waitForTimeout(50);
      check(await modal.isVisible(), `${signal}: IME must keep the modal open`);
      check((await page.evaluate(() => window.modelBudgetShortcutFixture.snapshot().calls.length)) === mark, `${signal}: IME must not resolve or save`);
    }
    await page.keyboard.press(scenario.shortcut);
    await modal.waitFor({ state: 'detached' });
    const saved = await page.evaluate(() => window.modelBudgetShortcutFixture.snapshot());
    const submitCalls = saved.calls.slice(mark);
    const resolves = submitCalls.filter(call => call.command === 'resolve_model_budget');
    check(resolves.length === 1, 'shortcut executes one submit-time resolver call');
    check(resolves[0].args.contextTokens === scenario.after, 'shortcut uses the changed configuredContextTokens, not its opening value');
    check(resolves[0].args.config.apiKey === 'fixture-only', 'observed submit validation, not a sanitized preview call');
    check(submitCalls.findIndex(call => call.command === 'resolve_model_budget') < submitCalls.findIndex(call => call.command === 'db_save'), 'validation precedes real store persistence');
    check(submitCalls.filter(call => call.command === 'db_save').length === 1, 'real update action saves once');
    check(saved.rows[0].max_tokens === 12345 && saved.rows[0].output_budget_mode === 'manual', 'IPC persistence retains exact output and mode');
    check(saved.rows[0].context_window_tokens === 1000000, 'model window stays separate from the changed global cap');
    check(saved.contextTokens === scenario.after, 'global settings retain the new cap');
    const reopened = await page.evaluate(() => window.modelBudgetShortcutFixture.reloadModels());
    check(reopened.models[0].maxTokens === 12345 && reopened.models[0].outputBudgetMode === 'manual', 'real storage reload roundtrips the saved fields');
    console.log(`PASS ${scenario.locale}: ${scenario.before} -> ${scenario.after}, ${scenario.shortcut}, both IME guards, store save/reload`);
  }
  await page.evaluate(() => window.modelBudgetShortcutFixture.reset('zh-CN', 48000));
  const labels = await page.evaluate(() => window.modelBudgetShortcutFixture.labels());
  await page.locator('.ai-model-actions-tools button[aria-haspopup="menu"]').click();
  await page.getByRole('menuitem', { name: labels.edit, exact: true }).click();
  const modal = page.locator('.models-modal');
  const context = modal.locator('.ai-budget-controls > :first-child');
  const output = modal.locator('.ai-budget-controls > :nth-child(2)');
  await modal.locator('.ai-budget-preview[aria-busy="false"]').waitFor();
  check(await modal.getByRole('slider').count() === 2, 'real modal shows both manual sliders');
  const sliderScreenshot = resolve(scratch, 'model-budget-sliders.png');
  await modal.locator('.ai-budget-settings').screenshot({ path: sliderScreenshot });
  console.log(`Slider screenshot: ${sliderScreenshot}`);
  const contextSlider = context.getByRole('slider');
  await contextSlider.focus();
  await page.keyboard.press('Home');
  check(await context.getByRole('spinbutton').inputValue() === '4096', 'native keyboard reaches context minimum');
  await page.keyboard.press('ArrowRight');
  check(Number(await context.getByRole('spinbutton').inputValue()) > 4096, 'native keyboard increases context tokens');
  for (const field of [context, output]) {
    const slider = field.getByRole('slider');
    await slider.scrollIntoViewIfNeeded();
    const box = await slider.boundingBox();
    await page.mouse.move(box.x + box.width * 0.5, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.65, box.y + box.height / 2, { steps: 8 });
    await page.mouse.up();
    const value = Number(await field.getByRole('spinbutton').inputValue());
    check(value > 4096 && value < 2000000, 'real pointer drag synchronizes token input');
  }
  await context.getByRole('spinbutton').fill('128001');
  await output.getByRole('spinbutton').fill('65537');
  check((await contextSlider.getAttribute('aria-valuetext')).includes('128,001'), 'precise context input updates accessible slider value');
  await output.getByRole('spinbutton').press('Control+Enter');
  await modal.waitFor({ state: 'detached' });
  const sliderSave = await page.evaluate(() => window.modelBudgetShortcutFixture.snapshot());
  check(sliderSave.rows[0].context_window_tokens === 128001 && sliderSave.rows[0].max_tokens === 65537,
    'slider controls retain precise values through the real save path');
  check(errors.length === 0, `no browser errors: ${errors.join('\n')}`);
  await page.evaluate(() => { document.querySelector('#fixture-status').textContent = 'PASS: real component shortcut regression'; });
  const screenshot = resolve(scratch, 'model-budget-shortcut.png');
  await page.screenshot({ path: screenshot, fullPage: true });
  console.log(`PASS model budget shortcut (${assertions} assertions)`);
  console.log(`Screenshot: ${screenshot}`);
} finally {
  await browser?.close();
  await server.close();
}
