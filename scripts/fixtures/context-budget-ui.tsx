import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import AskPanel from '../../src/components/aiHub/AskPanel';
import { contextMeter } from '../../src/components/aiHub/contextUsage';
import { ModelBudgetSettings } from '../../src/components/AIAssistant';
import type { AIModelConfig, ModelBudget } from '../../src/core/types';
import i18n from '../../src/i18n/config';
import '../../src/styles/index.css';

const root = createRoot(document.querySelector('#fixture')!);
const container = document.querySelector<HTMLElement>('#fixture')!;
const check = (value: unknown, message: string) => { if (!value) throw new Error(message); };
const noop = () => {};
const task = { sessionId:'budget', taskId:'task', status:'ended', reason:'completed', goal:'Review the budget', workspace:'D:/workspace', branch:'main', plan:[], actions:[], verification:{status:'unverified',command:null,exitCode:null} };
const budget = { model:'fixture', modelName:'Fixture model', modelWindow:48000, effectiveWindow:48000, windowSource:'provider', outputMode:'auto', outputSource:'unknown', requestedOutputTokens:8192, outputReserve:8192, schemaTokens:5841, inputBudget:33967, compactTrigger:27173, limitations:[] };
const info = { estimatedTokens:29838, inputBudget:33967, totalInputTokens:35679, covered:2, compacted:true, usageKind:'request', budget };
let version = 0;
let assertions = 0;
const assert = (value: unknown, message: string) => { assertions++; check(value, message); };
const render = (contextInfo: unknown, withTask = false) => flushSync(() => root.render(<AskPanel key={++version} {...{
  input:'', setInput:noop, inputRef:{current:null}, busy:false, phase:'idle', modelName:'Fixture', error:null, failedMode:null,
  onRetry:noop, onDismissError:noop, lastQuestion:'', onExample:noop, answer:'', selectedText:'', setSelectedText:noop,
  turns:[{role:'user',content:'Review the budget'},{role:'assistant',content:'Fixture answer'}], historyStart:0, legacySummary:'', contextInfo,
  agentTask:withTask ? task : null, onResumeTask:noop, projectLabel:null, agentSteps:[], agentPlan:[], onExport:noop, openList:null, permission:'read',
} as any} />));
const click = (selector: string) => flushSync(() => document.querySelector<HTMLButtonElement>(selector)!.click());
function SettingsFixture({ initial = {}, error, disabled = false }: { initial?: Partial<AIModelConfig>; error?: string; disabled?: boolean }) {
  const [config, setConfig] = useState({ maxTokens:32768, outputBudgetMode:'auto' as 'auto' | 'manual', contextWindowTokens:undefined as number | undefined, ...initial });
  return <ModelBudgetSettings config={config} budget={{ ...budget, modelWindow:1000000, windowSource:'catalog', modelMaxOutputTokens:384000, outputSource:'catalog', limitations:['model_output_limit'] } as ModelBudget}
    error={error} disabled={disabled} onChange={patch => setConfig(cur => ({ ...cur, ...patch }))} />;
}
const settings = (props: React.ComponentProps<typeof SettingsFixture> = {}) => flushSync(() => root.render(<SettingsFixture key={++version} {...props} />));
const enterNumber = (selector: string, value: string) => flushSync(() => {
  const input = document.querySelector<HTMLInputElement>(selector)!;
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
  input.dispatchEvent(new Event('input', {bubbles:true}));
});
const pressKey = (selector: string, key: string) => flushSync(() => {
  document.querySelector<HTMLInputElement>(selector)!.dispatchEvent(new KeyboardEvent('keydown', {key,bubbles:true,cancelable:true}));
});
try {
  for (const locale of ['zh-CN','en-US']) {
    await i18n.changeLanguage(locale);
    for (const width of [320,640,900]) {
      container.style.cssText = `width:${width}px;max-width:100%;height:800px;display:flex;flex-direction:column`;
      for (const withTask of [false,true]) {
        render(info, withTask);
        assert(document.querySelector('.qa-context-percent')?.textContent === '74%', 'percentage must use total input / full effective window');
        assert(document.querySelector<HTMLElement>('.qa-context-usage')?.dataset.level === 'high', 'color must use compaction pressure');
        const meter = document.querySelector<HTMLElement>('.qa-context-meter')!;
        assert(meter.firstElementChild?.getAttribute('style')?.includes('74%'), 'bar matches full-window percentage');
        if (withTask) click('.qa-task-disclosure');
        click('.qa-context-toggle');
        const panel = document.querySelector<HTMLElement>('.qa-context-panel')!;
        const next = document.querySelector<HTMLElement>(withTask ? '.qa-task-details' : '.qa-thread')!;
        assert(!panel.hidden && panel.scrollWidth <= panel.clientWidth, `details fit at ${width}/${locale}`);
        assert(panel.getBoundingClientRect().bottom <= next.getBoundingClientRect().top + 1, 'details remain in normal flow');
        assert(document.querySelectorAll('.qa-context-facts dt').length === 9, 'budget breakdown includes sources and all allocations');
        assert(panel.textContent?.includes('48,000') && panel.textContent.includes('8,192') && panel.textContent.includes('5,841'), 'declared capacity and reserves visible');
        if (withTask) {
          click('.qa-task-disclosure');
          assert(document.querySelector<HTMLElement>('.qa-task-details')?.hidden && !panel.hidden, 'task and usage disclosures are independent');
        }
      }
    }
  }
  for (const [used, content, window, expected, level] of [
    [0,0,48000,'0%','normal'], [24000,20000,48000,'50%','normal'], [35679,29838,48000,'74%','high'],
    [40000,35000,48000,'83%','full'], [60000,55000,48000,'125%','full'], [25241,20241,1000000,'3%','normal'],
  ] as const) {
    render({...info,totalInputTokens:used,estimatedTokens:content,budget:{...budget,effectiveWindow:window},inputBudget:window === 1000000 ? 986808 : 33967});
    assert(document.querySelector('.qa-context-percent')?.textContent === expected, `window percentage ${used}/${window}`);
    assert(document.querySelector<HTMLElement>('.qa-context-usage')?.dataset.level === level, 'pressure-based status');
    assert(document.querySelector<HTMLElement>('.qa-context-meter > span')?.style.width === `${Math.min(parseInt(expected),100)}%`, 'bar clamps visually at full');
  }
  for (const snapshot of [
    {estimatedTokens:20241,inputBudget:33967,covered:2,compacted:false},
    {...info,usageKind:'compaction'},
    {...info,totalInputTokens:null}, {...info,totalInputTokens:NaN}, {...info,budget:{...budget,effectiveWindow:0}},
  ]) {
    render(snapshot);
    assert(!document.querySelector('.qa-context-percent'), 'incomplete or historical snapshots must not invent a window percentage');
    click('.qa-context-toggle');
    assert(!document.querySelector('.qa-context-panel')?.hasAttribute('hidden'), 'historical details can still be opened');
  }
  assert(contextMeter(null) === null, 'missing snapshot');
  for (const locale of ['zh-CN','en-US']) {
    await i18n.changeLanguage(locale);
    for (const width of [320,640,900]) {
      container.style.cssText = `width:${width}px;max-width:100%;height:auto;display:block`;
      settings();
      assert(document.querySelectorAll('[role=radio][aria-checked=true]').length === 2, 'context and output start automatic');
      assert(!document.querySelector('.ai-budget-number'), 'automatic mode hides manual values');
      assert(!document.querySelector('.ai-token-slider'), 'automatic mode hides manual sliders');
      const panel = document.querySelector<HTMLElement>('.ai-budget-settings')!;
      assert(panel.scrollWidth <= panel.clientWidth, `settings fit at ${width}/${locale}`);
      assert(document.querySelectorAll('.ai-budget-source').length === 2, 'window and output capability each show their source');
      assert(Boolean(document.querySelector('.ai-budget-reasons li')?.textContent), 'adjustment reasons visible');
      click('.ai-budget-controls > :nth-child(2) [role=radio]:last-child');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '32768', 'switching manual preserves exact prior cap');
      const initialPosition = document.querySelector<HTMLInputElement>('.ai-token-slider')!.valueAsNumber;
      enterNumber('.ai-budget-number','65537');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '65537', 'manual cap accepts an exact integer beyond preset sizes');
      assert(document.querySelector<HTMLInputElement>('.ai-token-slider')!.valueAsNumber > initialPosition, 'precise input updates the slider position');
      assert(document.querySelector('.ai-token-slider')?.getAttribute('aria-valuetext')?.includes('65,537'), 'slider announces the exact token count');
      enterNumber('.ai-token-slider','500');
      assert(Number(document.querySelector<HTMLInputElement>('.ai-budget-number')!.value) > 1, 'dragging output updates the numeric field');
      pressKey('.ai-token-slider','Home');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '1', 'output slider reaches minimum');
      pressKey('.ai-token-slider','ArrowRight');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '2', 'keyboard can increase the smallest output without getting stuck');
      enterNumber('.ai-budget-number','4194305');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '4194305', 'slider does not clamp a larger legacy manual output');
      enterNumber('.ai-token-slider','500');
      pressKey('.ai-token-slider','End');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '4194305', `expanded output range stays stable after dragging down: ${document.querySelector<HTMLInputElement>('.ai-budget-number')?.value}`);
      click('.ai-budget-restore');
      assert(!document.querySelector('.ai-budget-number'), 'restore output automation');
      click('.ai-budget-controls > :first-child [role=radio]:last-child');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '1000000', 'manual window starts from resolved model capability');
      enterNumber('.ai-budget-number','128001');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '128001', 'manual window accepts exact value');
      pressKey('.ai-token-slider','End');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '2000000', 'context slider reaches its allowed maximum');
      pressKey('.ai-token-slider','Home');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '4096', 'context slider reaches its allowed minimum');
      pressKey('.ai-token-slider','ArrowLeft');
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '4096', 'context keyboard adjustment respects the minimum');
      assert(panel.scrollWidth <= panel.clientWidth, `manual sliders and inputs fit at ${width}/${locale}`);
      click('.ai-budget-restore');
      assert(!document.querySelector('.ai-budget-number'), 'restore window follows model');
      settings({initial:{outputBudgetMode:undefined,maxTokens:12345}});
      assert(document.querySelector<HTMLInputElement>('.ai-budget-number')?.value === '12345', 'legacy config remains manual without rewriting its cap');
      settings({error:'CONTEXT_OUTPUT_TOO_LARGE'});
      assert(document.querySelector('[role=alert]')?.textContent?.includes('CONTEXT_OUTPUT_TOO_LARGE'), 'invalid budget shows the backend reason');
      settings({disabled:true,initial:{contextWindowTokens:48000,outputBudgetMode:'manual'}});
      assert([...document.querySelectorAll<HTMLButtonElement>('.ai-budget-settings button')].every(button => button.disabled), 'busy controls cannot overwrite edits');
      assert(document.querySelectorAll('.ai-token-slider:disabled').length === 2 && document.querySelectorAll('.ai-budget-number:disabled').length === 2, 'busy state disables sliders and precise inputs together');
    }
  }
  await i18n.changeLanguage('zh-CN');
  container.style.width = '640px';
  render(info,true);
  click('.qa-task-disclosure');
  click('.qa-context-toggle');
  document.title = `PASS context budget UI (${assertions} assertions)`;
} catch (error) {
  document.title = `FAIL ${error instanceof Error ? error.stack : String(error)}`;
}
