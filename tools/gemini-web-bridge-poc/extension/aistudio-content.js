(function runCodexBarAiStudioSpendBridge() {
  const SPEND_MESSAGE = 'codexbar:gemini-api-spend:summary';
  const REFRESH_MESSAGE = 'codexbar:gemini-api-spend:refresh';

  function parseAndSend() {
    if (location.origin !== 'https://aistudio.google.com' || location.pathname !== '/spend') return;
    const result = CodexBarAiStudioSpendParser.parseSpendText(document.body?.innerText || '');
    if (!result) return;
    chrome.runtime.sendMessage({
      type: SPEND_MESSAGE,
      version: 1,
      provider: 'gemini-api',
      observed_at: Math.floor(Date.now() / 1000),
      payload: result
    }, () => void chrome.runtime.lastError);
  }

  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) parseAndSend();
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) parseAndSend();
  });
  setTimeout(parseAndSend, 250);
  setTimeout(parseAndSend, 1500);
})();
