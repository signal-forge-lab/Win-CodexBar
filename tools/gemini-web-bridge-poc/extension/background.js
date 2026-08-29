const HOST_NAME = 'com.codexbar.gemini_web_bridge_poc';
const QUOTA_MESSAGE = 'codexbar:gemini-apps-poc:quota';
const REFRESH_MESSAGE = 'codexbar:gemini-apps-poc:refresh';
const CACHE_KEY = 'codexbarGeminiAppsPocLastPush';
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

function refreshGeminiTab(tab) {
  if (tab?.id == null) return;
  chrome.tabs.update(tab.id, { autoDiscardable: false }, () => {
    if (chrome.runtime.lastError) return;
    if (tab.discarded || tab.frozen) {
      chrome.tabs.reload(tab.id, () => void chrome.runtime.lastError);
      return;
    }
    chrome.tabs.sendMessage(tab.id, { type: REFRESH_MESSAGE }, () => {
      void chrome.runtime.lastError;
    });
  });
}

function refreshOpenGeminiTabs() {
  chrome.tabs.query({ url: ['https://gemini.google.com/*'] }, (tabs) => {
    void chrome.runtime.lastError;
    for (const tab of tabs || []) refreshGeminiTab(tab);
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
  if (!validMessage(message, sender)) return;
  chrome.storage.local.set({ [CACHE_KEY]: message }, () => void chrome.runtime.lastError);
  void forward(message);
});

chrome.action.onClicked.addListener(() => {
  connectNative();
  void chrome.storage.local.get(CACHE_KEY).then(async (stored) => {
    if (stored[CACHE_KEY]) await forward(stored[CACHE_KEY]);
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

void restoreConnection();
void ensureRefreshAlarm();
