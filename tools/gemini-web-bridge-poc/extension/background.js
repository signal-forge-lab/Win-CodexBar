const HOST_NAME = 'com.codexbar.gemini_web_bridge_poc';
const QUOTA_MESSAGE = 'codexbar:gemini-apps-poc:quota';
const SPEND_MESSAGE = 'codexbar:gemini-api-spend:summary';
const REFRESH_MESSAGE = 'codexbar:gemini-apps-poc:refresh';
const SPEND_REFRESH_MESSAGE = 'codexbar:gemini-api-spend:refresh';
const CACHE_KEY = 'codexbarGeminiAppsPocLastPush';
const SPEND_CACHE_KEY = 'codexbarGeminiApiSpendLastPush';
const MANAGED_TAB_KEY = 'codexbarGeminiAppsManagedTabId';
const MANAGED_SPEND_TAB_KEY = 'codexbarGeminiApiSpendManagedTabId';
const REFRESH_ALARM = 'codexbar-gemini-apps-poc-refresh';
const REFRESH_INTERVAL_MINUTES = 3;

let nativePort = null;

function validWindow(window, label) {
  return window
    && window.label === label
    && Number.isFinite(window.used_percent)
    && window.used_percent >= 0
    && window.used_percent <= 100
    && (window.resets_at == null || typeof window.resets_at === 'string');
}

function validMessage(message, sender) {
  const url = sender?.url || '';
  const payload = message?.payload;
  return message?.type === QUOTA_MESSAGE
    && message.version === 1
    && message.provider === 'gemini-apps'
    && sender?.frameId === 0
    && url.startsWith('https://gemini.google.com/')
    && Number.isInteger(message.observed_at)
    && message.observed_at > 0
    && payload
    && /^\d+$/.test(payload.account_id || '')
    && ['jSf9Qc', 'VxUbXb', 'dom'].includes(payload.source)
    && (payload.plan == null || typeof payload.plan === 'string')
    && validWindow(payload.current, 'Current usage')
    && validWindow(payload.weekly, 'Weekly limit');
}

function validSpendPayload(payload) {
  return payload
    && Number.isFinite(payload.used)
    && payload.used >= 0
    && (payload.cap_used == null || (Number.isFinite(payload.cap_used) && payload.cap_used >= 0))
    && (payload.limit == null || (Number.isFinite(payload.limit) && payload.limit >= 0))
    && (payload.limit == null || payload.cap_used != null)
    && /^(USD|EUR|GBP|JPY)$/.test(payload.currency || '')
    && typeof payload.period === 'string'
    && payload.period.length > 0
    && payload.period.length <= 64
    && payload.resets_at == null
    && (payload.scope == null
      || (typeof payload.scope === 'string' && payload.scope.length > 0
        && payload.scope.length <= 64 && !payload.scope.includes('@')))
    && payload.source === 'dom'
    && Object.keys(payload).every((key) =>
      ['used', 'cap_used', 'limit', 'currency', 'period', 'resets_at', 'scope', 'source'].includes(key));
}

function validSpendMessage(message, sender) {
  const payload = message?.payload;
  let url;
  try {
    url = new URL(sender?.url || '');
  } catch (_) {
    return false;
  }
  return message?.type === SPEND_MESSAGE
    && message.version === 1
    && message.provider === 'gemini-api'
    && sender?.frameId === 0
    && url.origin === 'https://aistudio.google.com'
    && url.pathname === '/spend'
    && Number.isInteger(message.observed_at)
    && message.observed_at > 0
    && validSpendPayload(payload);
}

function connectNative() {
  if (nativePort) return nativePort;
  try {
    nativePort = chrome.runtime.connectNative(HOST_NAME);
    nativePort.onMessage.addListener(() => {});
    nativePort.onDisconnect.addListener(() => {
      void chrome.runtime.lastError;
      nativePort = null;
    });
    return nativePort;
  } catch (_) {
    nativePort = null;
    return null;
  }
}

async function forward(message) {
  const port = connectNative();
  if (!port) return;
  const { type, ...wire } = message;
  try {
    port.postMessage(wire);
  } catch (_) {
    nativePort = null;
  }
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function captureAiStudioSpend(tabId) {
  try {
    await chrome.scripting.executeScript({
      target: { tabId },
      files: ['aistudio-parser.js']
    });
    for (let attempt = 0; attempt < 12; attempt += 1) {
      const result = await chrome.scripting.executeScript({
        target: { tabId },
        func: () => globalThis.CodexBarAiStudioSpendParser
          ?.parseSpendText(document.body?.innerText || '') ?? null
      });
      const payload = result?.[0]?.result ?? null;
      if (validSpendPayload(payload)) {
        const message = {
          type: SPEND_MESSAGE,
          version: 1,
          provider: 'gemini-api',
          observed_at: Math.floor(Date.now() / 1000),
          payload
        };
        await chrome.storage.local.set({ [SPEND_CACHE_KEY]: message });
        await forward(message);
        return true;
      }
      await sleep(500);
    }
  } catch (_) {
    // Browser-bridge capture is best effort. The native provider can fall back to local CDP.
  }
  return false;
}

function refreshGeminiTab(tab) {
  if (tab?.id == null) return;
  chrome.tabs.update(tab.id, { autoDiscardable: false }, () => {
    if (chrome.runtime.lastError) return;
    if (tab.discarded || tab.frozen) {
      chrome.tabs.reload(tab.id, () => void chrome.runtime.lastError);
      return;
    }
    const type = tab.url?.startsWith('https://aistudio.google.com/spend')
      ? SPEND_REFRESH_MESSAGE
      : REFRESH_MESSAGE;
    if (type === SPEND_REFRESH_MESSAGE) {
      void captureAiStudioSpend(tab.id);
    }
    chrome.tabs.sendMessage(tab.id, { type }, () => {
      void chrome.runtime.lastError;
    });
  });
}

function refreshOpenGeminiTabs() {
  chrome.tabs.query({ url: ['https://gemini.google.com/*', 'https://aistudio.google.com/spend*'] }, (tabs) => {
    if (chrome.runtime.lastError) return;
    const matching = tabs || [];
    for (const tab of matching) refreshGeminiTab(tab);
    if (!matching.some((tab) => tab.url?.startsWith('https://gemini.google.com/'))) {
      ensureManagedTab(MANAGED_TAB_KEY, createManagedUsageTab);
    }
    if (!matching.some((tab) => tab.url?.startsWith('https://aistudio.google.com/spend'))) {
      ensureManagedTab(MANAGED_SPEND_TAB_KEY, createManagedSpendTab);
    }
  });
}

function ensureManagedTab(storageKey, createTab) {
  chrome.storage.local.get(storageKey, (stored) => {
    if (chrome.runtime.lastError) return;
    const managedTabId = stored[storageKey];
    if (Number.isInteger(managedTabId)) {
      chrome.tabs.get(managedTabId, () => {
        if (!chrome.runtime.lastError) return;
        chrome.storage.local.remove(storageKey, () => void chrome.runtime.lastError);
        createTab();
      });
      return;
    }
    createTab();
  });
}

function createManagedUsageTab() {
  chrome.tabs.create({ url: 'https://gemini.google.com/usage', active: false }, (tab) => {
    if (chrome.runtime.lastError || tab?.id == null) return;
    chrome.storage.local.set({ [MANAGED_TAB_KEY]: tab.id }, () => void chrome.runtime.lastError);
    chrome.tabs.update(tab.id, { autoDiscardable: false }, () => void chrome.runtime.lastError);
  });
}

function createManagedSpendTab() {
  chrome.tabs.create({ url: 'https://aistudio.google.com/spend', active: false }, (tab) => {
    if (chrome.runtime.lastError || tab?.id == null) return;
    chrome.storage.local.set({ [MANAGED_SPEND_TAB_KEY]: tab.id }, () => void chrome.runtime.lastError);
    chrome.tabs.update(tab.id, { autoDiscardable: false }, () => void chrome.runtime.lastError);
  });
}

async function ensureRefreshAlarm() {
  const alarm = await chrome.alarms.get(REFRESH_ALARM);
  if (!alarm) {
    await chrome.alarms.create(REFRESH_ALARM, {
      periodInMinutes: REFRESH_INTERVAL_MINUTES
    });
  }
}

async function restoreConnection() {
  connectNative();
  refreshOpenGeminiTabs();
}

chrome.runtime.onMessage.addListener((message, sender) => {
  const cacheKey = validMessage(message, sender)
    ? CACHE_KEY
    : validSpendMessage(message, sender)
      ? SPEND_CACHE_KEY
      : null;
  if (!cacheKey) return;
  chrome.storage.local.set({ [cacheKey]: message }, () => void chrome.runtime.lastError);
  void forward(message);
});

chrome.action.onClicked.addListener(() => {
  connectNative();
  void chrome.storage.local.get([CACHE_KEY, SPEND_CACHE_KEY]).then(async (stored) => {
    if (stored[CACHE_KEY]) await forward(stored[CACHE_KEY]);
    if (stored[SPEND_CACHE_KEY]) await forward(stored[SPEND_CACHE_KEY]);
    refreshOpenGeminiTabs();
  });
});

chrome.runtime.onStartup.addListener(() => {
  void restoreConnection();
  void ensureRefreshAlarm();
});

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === REFRESH_ALARM) refreshOpenGeminiTabs();
});

chrome.idle.onStateChanged.addListener((state) => {
  if (state === 'active') refreshOpenGeminiTabs();
});

chrome.tabs.onRemoved.addListener((tabId) => {
  chrome.storage.local.get([MANAGED_TAB_KEY, MANAGED_SPEND_TAB_KEY], (stored) => {
    if (chrome.runtime.lastError) return;
    for (const key of [MANAGED_TAB_KEY, MANAGED_SPEND_TAB_KEY]) {
      if (stored[key] === tabId) {
        chrome.storage.local.remove(key, () => void chrome.runtime.lastError);
      }
    }
  });
});

chrome.tabs.onUpdated.addListener((_tabId, changeInfo, tab) => {
  if (changeInfo.status !== 'complete') return;
  const url = tab?.url || '';
  if (url.startsWith('https://gemini.google.com/')
      || url.startsWith('https://aistudio.google.com/spend')) {
    refreshGeminiTab(tab);
  }
});

void restoreConnection();
void ensureRefreshAlarm();
