(function runCodexBarGeminiAppsPoc() {
  const STATE_MESSAGE = 'CODEXBAR_GEMINI_APPS_POC_STATE';
  const REFRESH_STATE_MESSAGE = 'CODEXBAR_GEMINI_APPS_POC_REFRESH_STATE';
  const QUOTA_MESSAGE = 'codexbar:gemini-apps-poc:quota';
  const REFRESH_MESSAGE = 'codexbar:gemini-apps-poc:refresh';
  const accountId = (location.pathname.match(/^\/u\/(\d+)(?:\/|$)/) || [])[1] || '0';
  const accountPrefix = accountId === '0' ? '' : `/u/${accountId}`;

  let pageState = null;
  let fetchInFlight = false;

  function detectPlanFromDom(text) {
    const leafText = Array.from(document.querySelectorAll('*'))
      .filter((element) => element.children.length === 0)
      .map((element) => (element.textContent || '').trim().toUpperCase());
    if (leafText.includes('ULTRA')) return 'Ultra';
    if (leafText.includes('PRO')) return 'Pro';
    if (leafText.includes('PLUS')) return 'Plus';
    if (/Get 2x more usage with AI Plus|Get Google AI Plus/i.test(text)) return 'Free';
    return null;
  }

  async function callRpc(rpcId, state) {
    const query = new URLSearchParams({
      rpcids: rpcId,
      'source-path': '/usage'
    });
    if (rpcId === 'VxUbXb') {
      query.set('_reqid', String(Date.now() % 10000000));
      query.set('rt', 'c');
      if (state.bl) query.set('bl', state.bl);
      if (state.sid) query.set('f.sid', state.sid);
    }
    const url = `https://gemini.google.com${accountPrefix}/_/BardChatUi/data/batchexecute?${query}`;
    const body = new URLSearchParams({
      at: state.at,
      'f.req': JSON.stringify([[[rpcId, '[]', null, 'generic']]])
    });
    const response = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8' },
      body: body.toString(),
      credentials: 'include'
    });
    if (!response.ok) throw new Error(`Gemini ${rpcId} returned ${response.status}`);
    return response.text();
  }

  function sendSanitized(result) {
    const pageText = document.body?.innerText || '';
    const plan = result.plan || detectPlanFromDom(pageText);
    chrome.runtime.sendMessage({
      type: QUOTA_MESSAGE,
      version: 1,
      provider: 'gemini-apps',
      observed_at: Math.floor(Date.now() / 1000),
      payload: {
        account_id: accountId,
        plan,
        source: result.source,
        current: result.current,
        weekly: result.weekly
      }
    }, () => void chrome.runtime.lastError);
  }

  async function fetchAndSend() {
    if (fetchInFlight) return;
    if (!pageState?.at) {
      window.postMessage({ type: REFRESH_STATE_MESSAGE }, '*');
      return;
    }
    fetchInFlight = true;
    try {
      let result = null;
      try {
        result = CodexBarGeminiAppsPocParser.parseJSf9Qc(await callRpc('jSf9Qc', pageState));
      } catch (_) {}
      if (!result) {
        try {
          result = CodexBarGeminiAppsPocParser.parseVxUbXb(await callRpc('VxUbXb', pageState));
        } catch (_) {}
      }
      if (!result) {
        result = CodexBarGeminiAppsPocParser.parseDomText(document.body?.innerText || '');
      }
      if (result) sendSanitized(result);
    } finally {
      fetchInFlight = false;
    }
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.data?.type !== STATE_MESSAGE) return;
    const state = event.data.state;
    if (!state || typeof state.at !== 'string' || !state.at) return;
    pageState = {
      at: state.at,
      sid: typeof state.sid === 'string' ? state.sid : null,
      bl: typeof state.bl === 'string' ? state.bl : null
    };
    void fetchAndSend();
  });

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) void fetchAndSend();
  });
  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) void fetchAndSend();
  });
  window.postMessage({ type: REFRESH_STATE_MESSAGE }, '*');
})();
