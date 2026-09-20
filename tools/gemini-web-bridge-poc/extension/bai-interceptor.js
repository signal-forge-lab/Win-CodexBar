(function runCodexBarBaiUsageInterceptor() {
  if (globalThis.__codexbarBaiUsageInterceptorInstalled) return;
  globalThis.__codexbarBaiUsageInterceptorInstalled = true;

  const SNAPSHOT_MESSAGE = 'CODEXBAR_BAI_USAGE_SNAPSHOT';
  const REFRESH_MESSAGE = 'CODEXBAR_BAI_USAGE_REFRESH';
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

  async function getJson(path) {
    const response = await fetch(path, {
      credentials: 'include',
      cache: 'no-store',
      headers: { Accept: 'application/json' }
    });
    if (!response.ok) throw new Error(`b.ai bridge fetch failed: ${response.status}`);
    return response.json();
  }

  async function fetchSnapshot() {
    if (inFlight || !onBai()) return;
    inFlight = true;
    try {
      const parser = globalThis.CodexBarBaiUsageParser;
      if (!parser) return;
      const empty = emptyInput();
      const [pointsRaw, summaryRaw] = await Promise.all([
        getJson('/trpc/lambda/usage.points?batch=1&input=' + empty),
        getJson('/trpc/lambda/usage.summary?batch=1&input=' + empty)
      ]);
      const points = parser.parsePoints(pointsRaw);
      const summary = parser.parseSummary(summaryRaw);
      if (!points || !summary) return;

      const records = [];
      for (let page = 1; page <= MAX_PAGES; page += 1) {
        const raw = await getJson(
          '/trpc/lambda/order.listOrders?batch=1&input=' + ordersInput(page)
        );
        const parsed = parser.parseOrders(raw);
        if (!parsed) return;
        records.push(...parsed.data);
        if (page * PAGE_SIZE >= parsed.total) break;
        if (page === MAX_PAGES) return;
      }
      const funding = parser.fundingFromOrders(records);
      const payload = parser.buildPayload(points, summary, funding);
      if (!payload) return;
      window.postMessage({ type: SNAPSHOT_MESSAGE, payload }, location.origin);
    } catch (_) {
      // Best effort. The last-known-good secret-free cache remains usable.
    } finally {
      inFlight = false;
    }
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.origin !== location.origin) return;
    if (event.data?.type === REFRESH_MESSAGE) void fetchSnapshot();
  });
  setTimeout(() => void fetchSnapshot(), 250);
})();
