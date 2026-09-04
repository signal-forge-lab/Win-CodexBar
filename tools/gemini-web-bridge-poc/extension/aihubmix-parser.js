(function installCodexBarAiHubMixParser(root, factory) {
  const parser = factory();
  root.CodexBarAiHubMixRechargeParser = parser;
  if (typeof module !== 'undefined' && module.exports) module.exports = parser;
})(typeof globalThis !== 'undefined' ? globalThis : this, function buildParser() {
  const QUOTA_PER_USD = 500000;

  function numberValue(value) {
    if (typeof value === 'number') return Number.isFinite(value) ? value : null;
    if (typeof value !== 'string') return null;
    const parsed = Number(value.trim());
    return Number.isFinite(parsed) ? parsed : null;
  }

  function integerValue(value) {
    const parsed = numberValue(value);
    return Number.isInteger(parsed) ? parsed : null;
  }

  function parseDisplayAmount(value) {
    if (typeof value !== 'string') return null;
    const normalized = value
      .replace(/,/g, '')
      .replace(/\u00a0/g, ' ')
      .trim()
      .replace(/^\+\s*/, '');
    const match = normalized.match(/(?:^|\s|[$€£¥￥])([0-9]+(?:\.[0-9]+)?)(?:\s*(?:USD))?(?:$|\s)/i);
    if (!match) return null;
    const amount = Number(match[1]);
    return Number.isFinite(amount) && amount >= 0 ? amount : null;
  }

  function latestFundingFromResponse(response) {
    const records = response?.success === false ? null : response?.data;
    if (!Array.isArray(records)) return null;
    let best = null;
    for (const record of records) {
      const grantType = integerValue(record?.grant_type);
      const status = integerValue(record?.status);
      const quota = numberValue(record?.quota);
      const balanceAfter = numberValue(record?.balance_after);
      const createdTime = integerValue(record?.created_time) || 0;
      if (grantType === 4 || status !== 1 || quota == null || quota <= 0
          || balanceAfter == null || balanceAfter <= 0) continue;
      if (!best || createdTime > best.createdTime) best = { createdTime, balanceAfter };
    }
    if (!best) return null;
    const fundedBalance = best.balanceAfter / QUOTA_PER_USD;
    if (!Number.isFinite(fundedBalance) || fundedBalance <= 0) return null;
    return {
      funded_balance_usd: fundedBalance,
      funding_created_at: best.createdTime > 0 ? best.createdTime : null,
      source: 'network'
    };
  }

  function latestFundingFromRows(rows) {
    if (!Array.isArray(rows)) return null;
    for (const row of rows) {
      if (!Array.isArray(row)) continue;
      const quotaIndex = row.findIndex((cell) => typeof cell === 'string' && /^\s*\+/.test(cell));
      if (quotaIndex < 0 || quotaIndex + 1 >= row.length) continue;
      const quota = parseDisplayAmount(row[quotaIndex]);
      const balanceAfter = parseDisplayAmount(row[quotaIndex + 1]);
      if (quota == null || quota <= 0 || balanceAfter == null || balanceAfter <= 0) continue;
      return {
        funded_balance_usd: balanceAfter,
        funding_created_at: null,
        source: 'dom'
      };
    }
    return null;
  }

  return Object.freeze({ latestFundingFromResponse, latestFundingFromRows, parseDisplayAmount });
});
