import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import LanguageDetector from 'i18next-browser-languagedetector';

import zhCNCommon from '../locales/zh-CN/common.json';
import zhCNNav from '../locales/zh-CN/navigation.json';
import zhCNGit from '../locales/zh-CN/git.json';
import zhCNSettings from '../locales/zh-CN/settings.json';
import zhCNAi from '../locales/zh-CN/ai.json';
import zhCNHosts from '../locales/zh-CN/hosts.json';
import zhCNPlugins from '../locales/zh-CN/plugins.json';
import zhCNRuntime from '../locales/zh-CN/runtime.json';
import zhCNCloudflared from '../locales/zh-CN/cloudflared.json';
import zhCNAihub from '../locales/zh-CN/aihub.json';
import zhCNSnippets from '../locales/zh-CN/snippets.json';
import zhCNWorkTodo from '../locales/zh-CN/workTodo.json';
import zhCNDevtools from '../locales/zh-CN/devtools.json';
import zhCNUpdate from '../locales/zh-CN/update.json';
import zhCNWorkbuddy from '../locales/zh-CN/workbuddy.json';
import zhCNQuota from '../locales/zh-CN/quota.json';
import zhCNDesktopApps from '../locales/zh-CN/desktopApps.json';

import enUSCommon from '../locales/en-US/common.json';
import enUSNav from '../locales/en-US/navigation.json';
import enUSGit from '../locales/en-US/git.json';
import enUSSettings from '../locales/en-US/settings.json';
import enUSAi from '../locales/en-US/ai.json';
import enUSHosts from '../locales/en-US/hosts.json';
import enUSPlugins from '../locales/en-US/plugins.json';
import enUSRuntime from '../locales/en-US/runtime.json';
import enUSCloudflared from '../locales/en-US/cloudflared.json';
import enUSAihub from '../locales/en-US/aihub.json';
import enUSSnippets from '../locales/en-US/snippets.json';
import enUSWorkTodo from '../locales/en-US/workTodo.json';
import enUSDevtools from '../locales/en-US/devtools.json';
import enUSUpdate from '../locales/en-US/update.json';
import enUSWorkbuddy from '../locales/en-US/workbuddy.json';
import enUSQuota from '../locales/en-US/quota.json';
import enUSDesktopApps from '../locales/en-US/desktopApps.json';

const resources = {
  'zh-CN': {
    common: zhCNCommon,
    navigation: zhCNNav,
    git: zhCNGit,
    settings: zhCNSettings,
    ai: zhCNAi,
    hosts: zhCNHosts,
    plugins: zhCNPlugins,
    runtime: zhCNRuntime,
    cloudflared: zhCNCloudflared,
    aihub: zhCNAihub,
    snippets: zhCNSnippets,
    workTodo: zhCNWorkTodo,
    devtools: zhCNDevtools,
    update: zhCNUpdate,
    workbuddy: zhCNWorkbuddy,
    quota: zhCNQuota,
    desktopApps: zhCNDesktopApps,
  },
  'en-US': {
    common: enUSCommon,
    navigation: enUSNav,
    git: enUSGit,
    settings: enUSSettings,
    ai: enUSAi,
    hosts: enUSHosts,
    plugins: enUSPlugins,
    runtime: enUSRuntime,
    cloudflared: enUSCloudflared,
    aihub: enUSAihub,
    snippets: enUSSnippets,
    workTodo: enUSWorkTodo,
    devtools: enUSDevtools,
    update: enUSUpdate,
    workbuddy: enUSWorkbuddy,
    quota: enUSQuota,
    desktopApps: enUSDesktopApps,
  },
};

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources,
    fallbackLng: 'zh-CN',
    supportedLngs: ['zh-CN', 'en-US'],
    defaultNS: 'common',
    ns: ['common', 'navigation', 'git', 'settings', 'ai', 'hosts', 'plugins', 'runtime', 'cloudflared', 'aihub', 'snippets', 'drafts', 'devtools', 'update', 'workbuddy', 'quota', 'desktopApps'],

    detection: {
      order: ['localStorage', 'navigator'],
      caches: ['localStorage'],
      lookupLocalStorage: 'workbench-language',
    },

    interpolation: {
      escapeValue: false,
    },

    react: {
      useSuspense: false,
    },
  });

// Dev-only: locale JSONs are captured as snapshots when this module runs, so
// editing a translation afterwards would otherwise never reach the i18n
// singleton (stale labels / raw keys until a full restart). Self-accepting
// this module makes Vite re-run it on any locale change — init() runs again
// with the fresh bundles and react-i18next re-renders its subscribers.
if (import.meta.hot) {
  import.meta.hot.accept();
}

export default i18n;
