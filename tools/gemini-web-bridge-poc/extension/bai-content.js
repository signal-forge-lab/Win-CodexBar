(function runCodexBarBaiUsageBridge() {
  if (globalThis.__codexbarBaiUsageBridgeInstalled) return;
  globalThis.__codexbarBaiUsageBridgeInstalled = true;

  const WINDOW_MESSAGE = 'CODEXBAR_BAI_USAGE_SNAPSHOT';
  const WINDOW_REFRESH_MESSAGE = 'CODEXBAR_BAI_USAGE_REFRESH';
  const WINDOW_REFRESH_RESULT_MESSAGE = 'CODEXBAR_BAI_USAGE_REFRESH_RESULT';
  const USAGE_MESSAGE = 'codexbar:bai:usage';
  const REFRESH_MESSAGE = 'codexbar:bai:usage:refresh';
  const REFRESH_RESULT_MESSAGE = 'codexbar:bai:usage:refresh-result';
  const REFRESH_FAILURE_REASONS = new Set([
    'timeout', 'auth', 'rate-limit', 'server', 'fetch', 'parse'
  ]);

  function onBai() {
    return location.origin === 'https://chat.b.ai';
  }

  function validPayload(payload) {
    return payload
      && ['balance', 'bonus_remaining', 'monthly_spent', 'purchased_total',
        'bonus_total', 'funded_total'].every((key) =>
        Number.isFinite(payload[key]) && payload[key] >= 0)
      && payload.bonus_remaining <= payload.balance
      && Math.abs(payload.funded_total
        - payload.purchased_total - payload.bonus_total) <= 0.5
      && payload.source === 'network'
      && Object.keys(payload).every((key) =>
        ['balance', 'bonus_remaining', 'monthly_spent', 'purchased_total',
          'bonus_total', 'funded_total', 'source'].includes(key));
  }

  function requestRefresh() {
    if (!onBai()) return;
    window.postMessage({ type: WINDOW_REFRESH_MESSAGE }, location.origin);
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.origin !== location.origin) return;
    if (event.data?.type === WINDOW_MESSAGE && validPayload(event.data.payload)) {
      chrome.runtime.sendMessage({
        type: USAGE_MESSAGE,
        version: 1,
        provider: 'bai',
        observed_at: Math.floor(Date.now() / 1000),
        payload: event.data.payload
      }, () => void chrome.runtime.lastError);
      return;
    }
    if (event.data?.type !== WINDOW_REFRESH_RESULT_MESSAGE
        || typeof event.data.ok !== 'boolean') return;
    const reason = event.data.reason ?? null;
    if (reason != null && !REFRESH_FAILURE_REASONS.has(reason)) return;
    chrome.runtime.sendMessage({
      type: REFRESH_RESULT_MESSAGE,
      ok: event.data.ok,
      reason
    }, () => void chrome.runtime.lastError);
  });

  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) requestRefresh();
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) requestRefresh();
  });
  setTimeout(requestRefresh, 250);
})();
