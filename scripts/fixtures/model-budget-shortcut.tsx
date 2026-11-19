// Mount the real component/store/actions. Only the native IPC boundary is mocked.
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import type { AIModelConfig, ModelBudget } from '../../src/core/types';
import '../../src/styles/index.css';

type Call = { command: string; args: Record<string, unknown> };
const calls: Call[] = [];
let rows: Record<string, unknown>[] = [];
const model: AIModelConfig = {
  id: 'keyboard-fixture', name: 'Keyboard fixture', provider: 'custom',
  baseUrl: 'https://fixture.invalid/v1', apiKey: 'fixture-only', model: 'fixture-model',
  authType: 'api', temperature: 0.7, maxTokens: 12345, outputBudgetMode: 'manual',
  contextWindowTokens: 1000000, isDefault: true, vision: false, visionMode: 'text',
  createdAt: '', updatedAt: '',
};

// Fixed IPC response examples, not a second implementation of the Rust resolver.
const budgets: Record<number, ModelBudget> = {
  48000: {
    model: model.model, modelName: model.name, modelWindow: 1000000, windowSource: 'manual',
    configuredLimit: 48000, effectiveWindow: 48000, outputSource: 'unknown', outputMode: 'manual',
    requestedOutputTokens: 12345, outputReserve: 12345, schemaTokens: 0,
    inputBudget: 35655, compactTrigger: 28524, limitations: [],
  },
  24000: {
    model: model.model, modelName: model.name, modelWindow: 1000000, windowSource: 'manual',
    configuredLimit: 24000, effectiveWindow: 24000, outputSource: 'unknown', outputMode: 'manual',
    requestedOutputTokens: 12345, outputReserve: 12345, schemaTokens: 0,
    inputBudget: 11655, compactTrigger: 9324, limitations: [],
  },
  96000: {
    model: model.model, modelName: model.name, modelWindow: 1000000, windowSource: 'manual',
    configuredLimit: 96000, effectiveWindow: 96000, outputSource: 'unknown', outputMode: 'manual',
    requestedOutputTokens: 12345, outputReserve: 12345, schemaTokens: 0,
    inputBudget: 83655, compactTrigger: 66924, limitations: [],
  },
};

mockWindows('main');
mockIPC((command, payload) => {
  const args = structuredClone((payload ?? {}) as Record<string, unknown>);
  calls.push({ command, args });
  switch (command) {
    case 'resolve_model_budget': {
      const budget = budgets[Number(args.contextTokens)];
      if (!budget) throw new Error(`Unexpected fixture context limit: ${args.contextTokens}`);
      return structuredClone(budget);
    }
    case 'db_save':
      if (args.table !== 'ai_models') throw new Error(`Unexpected save: ${args.table}`);
      rows = structuredClone(args.rows as Record<string, unknown>[]);
      return;
    case 'db_load':
      if (args.table !== 'ai_models') throw new Error(`Unexpected load: ${args.table}`);
      return rows.map(columns => ({ columns: structuredClone(columns) }));
    default:
      throw new Error(`Unexpected native invoke: ${command}`);
  }
});

// Install the mocks before importing the component or global store.
const [{ default: AIAssistant }, { useGlobalStore }, { ConfirmDialogProvider }, { default: i18n }] = await Promise.all([
  import('../../src/components/AIAssistant'), import('../../src/core/store'),
  import('../../src/components/ConfirmModal'), import('../../src/i18n/config'),
]);
const root = createRoot(document.querySelector('#fixture')!);
let version = 0;
const fixture = {
  async reset(locale: string, contextTokens: number) {
    await i18n.changeLanguage(locale);
    calls.length = 0;
    rows = [];
    localStorage.setItem('workbench-models-page-view', 'list');
    flushSync(() => {
      useGlobalStore.setState(state => ({
        aiModels: [structuredClone(model)], snippets: [],
        settings: { ...state.settings, agentContextTokens: contextTokens },
      }));
      root.render(<ConfirmDialogProvider key={++version}><AIAssistant /></ConfirmDialogProvider>);
    });
  },
  setContextLimit(contextTokens: number) {
    // The same settings action used by the application; model/form state is untouched.
    useGlobalStore.getState().setSettings({ agentContextTokens: contextTokens });
  },
  snapshot() {
    return structuredClone({
      calls, rows, models: useGlobalStore.getState().aiModels,
      contextTokens: useGlobalStore.getState().settings.agentContextTokens,
    });
  },
  async reloadModels() {
    await useGlobalStore.getState().loadAIModels();
    return fixture.snapshot();
  },
  labels() {
    return {
      edit: i18n.t('models.edit', { ns: 'ai' }),
      maxTokens: i18n.t('models.maxTokens', { ns: 'ai' }),
      configuredCap: i18n.t('models.budgetConfiguredCap', { ns: 'ai' }),
    };
  },
};
Object.assign(window, { modelBudgetShortcutFixture: fixture });
document.querySelector('#fixture-status')!.textContent = 'Ready: real AIAssistant + global store + mock IPC';
