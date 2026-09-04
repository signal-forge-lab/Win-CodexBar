const HOST_NAME = 'com.codexbar.gemini_web_bridge_poc';
const QUOTA_MESSAGE = 'codexbar:gemini-apps-poc:quota';
const SPEND_MESSAGE = 'codexbar:gemini-api-spend:summary';
const AIHUBMIX_RECHARGE_MESSAGE = 'codexbar:aihubmix:recharge';
const REFRESH_MESSAGE = 'codexbar:gemini-apps-poc:refresh';
const SPEND_REFRESH_MESSAGE = 'codexbar:gemini-api-spend:refresh';
const AIHUBMIX_REFRESH_MESSAGE = 'codexbar:aihubmix:recharge:refresh';
const CACHE_KEY = 'codexbarGeminiAppsPocLastPush';
const SPEND_CACHE_KEY = 'codexbarGeminiApiSpendLastPush';
const AIHUBMIX_CACHE_KEY = 'codexbarAiHubMixRechargeLastPush';
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

function validAiHubMixPayload(payload) {
  return payload
    && Number.isFinite(payload.funded_balance_usd)
    && payload.funded_balance_usd > 0
    && (payload.funding_created_at == null
      || (Number.isInteger(payload.funding_created_at) && payload.funding_created_at > 0))
    && ['network', 'dom'].includes(payload.source)
    && Object.keys(payload).every((key) =>
      ['funded_balance_usd', 'funding_created_at', 'source'].includes(key));
}

function validAiHubMixMessage(message, sender) {
  let url;
  try {
    url = new URL(sender?.url || '');
  } catch (_) {
    return false;
  }
  return message?.type === AIHUBMIX_RECHARGE_MESSAGE
    && message.version === 1
    && message.provider === 'aihubmix'
    && sender?.frameId === 0
    && url.origin === 'https://console.aihubmix.com'
    && url.pathname === '/topup'
    && Number.isInteger(message.observed_at)
    && message.observed_at > 0
    && validAiHubMixPayload(message.payload);
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
    // Browser-bridge capture is best effort; the last-known-good cache remains available.
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
  });
}

function refreshAiHubMixTab(tab) {
  if (tab?.id == null) return;
  chrome.tabs.sendMessage(tab.id, { type: AIHUBMIX_REFRESH_MESSAGE }, () => {
    void chrome.runtime.lastError;
  });
}

function refreshOpenBridgeTabs() {
  refreshOpenGeminiTabs();
  chrome.tabs.query({ url: ['https://console.aihubmix.com/topup*'] }, (tabs) => {
    if (chrome.runtime.lastError) return;
    for (const tab of tabs || []) refreshAiHubMixTab(tab);
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
  refreshOpenBridgeTabs();
}

chrome.runtime.onMessage.addListener((message, sender) => {
  const cacheKey = validMessage(message, sender)
    ? CACHE_KEY
    : validSpendMessage(message, sender)
      ? SPEND_CACHE_KEY
      : validAiHubMixMessage(message, sender)
        ? AIHUBMIX_CACHE_KEY
      : null;
  if (!cacheKey) return;
  chrome.storage.local.set({ [cacheKey]: message }, () => void chrome.runtime.lastError);
  void forward(message);
});

chrome.action.onClicked.addListener(() => {
  connectNative();
  void chrome.storage.local.get([CACHE_KEY, SPEND_CACHE_KEY, AIHUBMIX_CACHE_KEY]).then(async (stored) => {
    if (stored[CACHE_KEY]) await forward(stored[CACHE_KEY]);
    if (stored[SPEND_CACHE_KEY]) await forward(stored[SPEND_CACHE_KEY]);
    if (stored[AIHUBMIX_CACHE_KEY]) await forward(stored[AIHUBMIX_CACHE_KEY]);
    refreshOpenBridgeTabs();
  });
});

chrome.runtime.onStartup.addListener(() => {
  void restoreConnection();
  void ensureRefreshAlarm();
});

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === REFRESH_ALARM) refreshOpenBridgeTabs();
});

chrome.idle.onStateChanged.addListener((state) => {
  if (state === 'active') refreshOpenBridgeTabs();
});

chrome.tabs.onUpdated.addListener((_tabId, changeInfo, tab) => {
  if (changeInfo.status !== 'complete') return;
  const url = tab?.url || '';
  if (url.startsWith('https://gemini.google.com/')
      || url.startsWith('https://aistudio.google.com/spend')) {
    refreshGeminiTab(tab);
  } else if (url.startsWith('https://console.aihubmix.com/topup')) {
    refreshAiHubMixTab(tab);
  }
});

void restoreConnection();
void ensureRefreshAlarm();
