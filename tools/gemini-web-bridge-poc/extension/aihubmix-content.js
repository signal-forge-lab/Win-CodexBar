(function runCodexBarAiHubMixRechargeBridge() {
  if (globalThis.__codexbarAiHubMixRechargeBridgeInstalled) return;
  globalThis.__codexbarAiHubMixRechargeBridgeInstalled = true;

  const WINDOW_MESSAGE = 'CODEXBAR_AIHUBMIX_RECHARGE_SNAPSHOT';
  const RECHARGE_MESSAGE = 'codexbar:aihubmix:recharge';
  const REFRESH_MESSAGE = 'codexbar:aihubmix:recharge:refresh';
  const INITIAL_OBSERVER_TIMEOUT_MS = 30_000;
  let observer = null;
  let scheduled = null;

  function onTopupPage() {
    return location.origin === 'https://console.aihubmix.com' && location.pathname === '/topup';
  }

  function validPayload(payload) {
    return payload
      && Number.isFinite(payload.funded_balance_usd)
      && payload.funded_balance_usd > 0
      && (payload.funding_created_at == null
        || (Number.isInteger(payload.funding_created_at) && payload.funding_created_at > 0))
      && ['network', 'dom'].includes(payload.source)
      && Object.keys(payload).every((key) =>
        ['funded_balance_usd', 'funding_created_at', 'source'].includes(key));
  }

  function send(payload) {
    if (!onTopupPage() || !validPayload(payload)) return false;
    chrome.runtime.sendMessage({
      type: RECHARGE_MESSAGE,
      version: 1,
      provider: 'aihubmix',
      observed_at: Math.floor(Date.now() / 1000),
      payload
    }, () => void chrome.runtime.lastError);
    return true;
  }

  function parseDom() {
    if (!onTopupPage()) return false;
    const rows = [...document.querySelectorAll('table tbody tr')].map((row) =>
      [...row.querySelectorAll('td')].map((cell) => cell.innerText || cell.textContent || ''));
    const payload = globalThis.CodexBarAiHubMixRechargeParser?.latestFundingFromRows(rows);
    return payload ? send(payload) : false;
  }

  function stopObserver() {
    observer?.disconnect();
    observer = null;
    if (scheduled != null) clearTimeout(scheduled);
    scheduled = null;
  }

  function scheduleParse() {
    if (scheduled != null) return;
    scheduled = setTimeout(() => {
      scheduled = null;
      if (parseDom()) stopObserver();
    }, 100);
  }

  function observeUntilSnapshot() {
    if (parseDom()) {
      stopObserver();
      return;
    }
    if (observer || !document.documentElement) return;
    observer = new MutationObserver(scheduleParse);
    observer.observe(document.documentElement, { subtree: true, childList: true, characterData: true });
    setTimeout(stopObserver, INITIAL_OBSERVER_TIMEOUT_MS);
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.origin !== location.origin) return;
    if (event.data?.type === WINDOW_MESSAGE) send(event.data.payload);
  });
  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) observeUntilSnapshot();
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) parseDom();
  });
  setTimeout(observeUntilSnapshot, 250);
})();
