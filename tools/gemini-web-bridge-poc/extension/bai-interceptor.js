(function runCodexBarBaiUsageInterceptor() {
  if (globalThis.__codexbarBaiUsageInterceptorInstalled) return;
  globalThis.__codexbarBaiUsageInterceptorInstalled = true;

  const SNAPSHOT_MESSAGE = 'CODEXBAR_BAI_USAGE_SNAPSHOT';
  const REFRESH_MESSAGE = 'CODEXBAR_BAI_USAGE_REFRESH';
  const REFRESH_RESULT_MESSAGE = 'CODEXBAR_BAI_USAGE_REFRESH_RESULT';
  const FETCH_TIMEOUT_MS = 15_000;
  const PAGE_SIZE = 100;
  const MAX_PAGES = 100;
  let inFlight = false;

  function onBai() {
    return location.origin === 'https://chat.b.ai';
  }

  function emptyInput() {
    return encodeURIComponent(JSON.stringify({
      0: { json: null, meta: { values: ['undefined'], v: 1 } }
    }));
  }

  function ordersInput(page) {
    return encodeURIComponent(JSON.stringify({
      0: {
        json: { page, pageSize: PAGE_SIZE, sortBy: 'createdAt', order: 'desc' }
      }
    }));
  }

  async function getJson(path, signal) {
    const response = await fetch(path, {
      credentials: 'include',
      cache: 'no-store',
      headers: { Accept: 'application/json' },
      signal
    });
    if (!response.ok) throw new Error(`b.ai bridge fetch failed: ${response.status}`);
    return response.json();
  }

  function failureReason(error) {
    if (error?.name === 'AbortError') return 'timeout';
    const message = String(error?.message || '');
    const statusMatch = message.match(/b\.ai bridge fetch failed: (\d+)/);
    if (statusMatch) {
      const status = Number(statusMatch[1]);
      if (status === 401 || status === 403) return 'auth';
      if (status === 429) return 'rate-limit';
      if (status >= 500) return 'server';
      return 'fetch';
    }
    if (/parser|parse|payload|orders|funding/i.test(message)) return 'parse';
    return 'fetch';
  }

  function reportRefreshResult(ok, reason = null) {
    window.postMessage({
      type: REFRESH_RESULT_MESSAGE,
      ok,
      reason
    }, location.origin);
  }

  async function fetchSnapshot(reportResult = false) {
    if (inFlight || !onBai()) return;
    inFlight = true;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    try {
      const parser = globalThis.CodexBarBaiUsageParser;
      if (!parser) throw new Error('b.ai parser unavailable');
      const empty = emptyInput();
      const [pointsRaw, summaryRaw] = await Promise.all([
        getJson('/trpc/lambda/usage.points?batch=1&input=' + empty, controller.signal),
        getJson('/trpc/lambda/usage.summary?batch=1&input=' + empty, controller.signal)
      ]);
      const points = parser.parsePoints(pointsRaw);
      const summary = parser.parseSummary(summaryRaw);
      if (!points || !summary) throw new Error('b.ai usage parse failed');

      const records = [];
      for (let page = 1; page <= MAX_PAGES; page += 1) {
        const raw = await getJson(
          '/trpc/lambda/order.listOrders?batch=1&input=' + ordersInput(page),
          controller.signal
        );
        const parsed = parser.parseOrders(raw);
        if (!parsed) throw new Error('b.ai orders parse failed');
        records.push(...parsed.data);
        if (page * PAGE_SIZE >= parsed.total) break;
        if (page === MAX_PAGES) throw new Error('b.ai orders exceeded page limit');
      }
      const funding = parser.fundingFromOrders(records);
      const payload = parser.buildPayload(points, summary, funding);
      if (!payload) throw new Error('b.ai payload build failed');
      window.postMessage({ type: SNAPSHOT_MESSAGE, payload }, location.origin);
      if (reportResult) reportRefreshResult(true);
    } catch (error) {
      if (reportResult) reportRefreshResult(false, failureReason(error));
    } finally {
      clearTimeout(timeout);
      inFlight = false;
    }
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.origin !== location.origin) return;
    if (event.data?.type === REFRESH_MESSAGE) void fetchSnapshot(true);
  });
  setTimeout(() => void fetchSnapshot(false), 250);
})();
