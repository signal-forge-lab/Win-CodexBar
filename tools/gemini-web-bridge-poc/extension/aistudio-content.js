(function runCodexBarAiStudioSpendBridge() {
  if (globalThis.__codexbarAiStudioSpendBridgeInstalled) return;
  globalThis.__codexbarAiStudioSpendBridgeInstalled = true;

  const SPEND_MESSAGE = 'codexbar:gemini-api-spend:summary';
  const REFRESH_MESSAGE = 'codexbar:gemini-api-spend:refresh';
  const INITIAL_OBSERVER_TIMEOUT_MS = 30_000;

  let initialObserver = null;
  let scheduledParse = null;

  function parseAndSend() {
    if (location.origin !== 'https://aistudio.google.com' || location.pathname !== '/spend') return false;
    const result = CodexBarAiStudioSpendParser.parseSpendText(document.body?.innerText || '');
    if (!result) return false;
    chrome.runtime.sendMessage({
      type: SPEND_MESSAGE,
      version: 1,
      provider: 'gemini-api',
      observed_at: Math.floor(Date.now() / 1000),
      payload: result
    }, () => void chrome.runtime.lastError);
    return true;
  }

  function stopInitialObserver() {
    initialObserver?.disconnect();
    initialObserver = null;
    if (scheduledParse != null) {
      clearTimeout(scheduledParse);
      scheduledParse = null;
    }
  }

  function tryObservedParse() {
    scheduledParse = null;
    if (parseAndSend()) stopInitialObserver();
  }

  function scheduleObservedParse() {
    if (scheduledParse != null) return;
    scheduledParse = setTimeout(tryObservedParse, 100);
  }

  function observeUntilFirstSnapshot() {
    if (parseAndSend()) {
      stopInitialObserver();
      return;
    }
    if (initialObserver || !document.documentElement) return;
    initialObserver = new MutationObserver(scheduleObservedParse);
    initialObserver.observe(document.documentElement, {
      subtree: true,
      childList: true,
      characterData: true
    });
    setTimeout(stopInitialObserver, INITIAL_OBSERVER_TIMEOUT_MS);
  }

  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) observeUntilFirstSnapshot();
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) parseAndSend();
  });
  setTimeout(observeUntilFirstSnapshot, 250);
})();
