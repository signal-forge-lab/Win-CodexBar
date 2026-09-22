const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const interceptorSource = fs.readFileSync(
  path.join(__dirname, '..', 'extension', 'bai-interceptor.js'),
  'utf8'
);

function loadInterceptor({ status = 200 } = {}) {
  const listeners = {};
  const posted = [];
  const window = {
    addEventListener(type, listener) {
      listeners[type] = listener;
    },
    postMessage(message) {
      posted.push(message);
    }
  };
  const parser = {
    parsePoints() {
      return { balance: 15_000_000, bonus_remaining: 5_000_000 };
    },
    parseSummary() {
      return { monthly_spent: 0 };
    },
    parseOrders() {
      return { data: [], total: 0 };
    },
    fundingFromOrders() {
      return { purchased_total: 10_000_000, bonus_total: 5_000_000, funded_total: 15_000_000 };
    },
    buildPayload() {
      return {
        balance: 15_000_000,
        bonus_remaining: 5_000_000,
        monthly_spent: 0,
        purchased_total: 10_000_000,
        bonus_total: 5_000_000,
        funded_total: 15_000_000,
        source: 'network'
      };
    }
  };
  const context = vm.createContext({
    window,
    location: { origin: 'https://chat.b.ai' },
    CodexBarBaiUsageParser: parser,
    AbortController,
    Promise,
    fetch: async () => ({
      ok: status >= 200 && status < 300,
      status,
      async json() { return {}; }
    }),
    setTimeout() { return 1; },
    clearTimeout() {}
  });
  vm.runInContext(interceptorSource, context, { filename: 'bai-interceptor.js' });
  return { listeners, posted, window };
}

async function flushAsyncWork() {
  await new Promise((resolve) => setImmediate(resolve));
  await new Promise((resolve) => setImmediate(resolve));
}

test('b.ai refresh reports server fetch failure to the extension', async () => {
  const { listeners, posted, window } = loadInterceptor({ status: 503 });
  listeners.message({
    source: window,
    origin: 'https://chat.b.ai',
    data: { type: 'CODEXBAR_BAI_USAGE_REFRESH' }
  });
  await flushAsyncWork();

  assert.deepEqual(posted, [{
    type: 'CODEXBAR_BAI_USAGE_REFRESH_RESULT',
    ok: false,
    reason: 'server'
  }]);
});

test('b.ai refresh reports success after publishing a fresh snapshot', async () => {
  const { listeners, posted, window } = loadInterceptor({ status: 200 });
  listeners.message({
    source: window,
    origin: 'https://chat.b.ai',
    data: { type: 'CODEXBAR_BAI_USAGE_REFRESH' }
  });
  await flushAsyncWork();

  assert.equal(posted.length, 2);
  assert.equal(posted[0].type, 'CODEXBAR_BAI_USAGE_SNAPSHOT');
  assert.deepEqual(posted[1], {
    type: 'CODEXBAR_BAI_USAGE_REFRESH_RESULT',
    ok: true,
    reason: null
  });
});
