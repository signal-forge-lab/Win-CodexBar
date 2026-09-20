(function installCodexBarBaiUsageParser(root, factory) {
  const api = factory();
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  if (root) root.CodexBarBaiUsageParser = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, () => {
  function unwrapBatch(value) {
    let parsed = value;
    if (typeof parsed === 'string') {
      try {
        parsed = JSON.parse(parsed);
      } catch (_) {
        return null;
      }
    }
    if (!Array.isArray(parsed) || parsed.length !== 1) return null;
    return parsed[0]?.result?.data?.json ?? null;
  }

  function finiteNonNegative(value) {
    return Number.isFinite(value) && value >= 0 ? value : null;
  }

  function parsePoints(value) {
    const json = unwrapBatch(value);
    if (!json) return null;
    const balance = finiteNonNegative(Number(json.points_balance));
    const bonusRemaining = finiteNonNegative(Number(json.points_expiring));
    if (balance == null || bonusRemaining == null || bonusRemaining > balance) return null;
    return { balance, bonus_remaining: bonusRemaining };
  }

  function parseSummary(value) {
    const json = unwrapBatch(value);
    if (!json) return null;
    const monthlySpent = finiteNonNegative(Number(json.monthly_spent));
    return monthlySpent == null ? null : { monthly_spent: monthlySpent };
  }

  function parseOrders(value) {
    const json = unwrapBatch(value);
    if (!json || !Array.isArray(json.data)) return null;
    const total = Number(json.total);
    if (!Number.isInteger(total) || total < 0) return null;
    return { data: json.data, total };
  }

  function fundingFromOrders(records) {
    if (!Array.isArray(records)) return null;
    let purchasedTotal = 0;
    let bonusTotal = 0;
    for (const record of records) {
      if (!record || record.status !== 'success') continue;
      const points = Number(record.points);
      if (!Number.isFinite(points) || points <= 0) continue;
      if (record.recipientRelation != null && record.recipientRelation !== 'self') continue;
      if (record.type === 'purchase' || record.rechargeType === 'fiat') {
        purchasedTotal += points;
      } else if (record.type === 'bonus' || record.rechargeType === 'bonus') {
        bonusTotal += points;
      }
    }
    return {
      purchased_total: purchasedTotal,
      bonus_total: bonusTotal,
      funded_total: purchasedTotal + bonusTotal
    };
  }

  function buildPayload(points, summary, funding) {
    if (!points || !summary || !funding) return null;
    const payload = {
      balance: points.balance,
      bonus_remaining: points.bonus_remaining,
      monthly_spent: summary.monthly_spent,
      purchased_total: funding.purchased_total,
      bonus_total: funding.bonus_total,
      funded_total: funding.funded_total,
      source: 'network'
    };
    if (!Object.values(payload).slice(0, 6).every((value) =>
      Number.isFinite(value) && value >= 0)) return null;
    if (payload.bonus_remaining > payload.balance) return null;
    if (Math.abs(payload.funded_total
      - payload.purchased_total - payload.bonus_total) > 0.5) return null;
    return payload;
  }

  return {
    unwrapBatch,
    parsePoints,
    parseSummary,
    parseOrders,
    fundingFromOrders,
    buildPayload
  };
});
