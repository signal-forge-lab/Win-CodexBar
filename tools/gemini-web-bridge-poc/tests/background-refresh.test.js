const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const backgroundSource = fs.readFileSync(
  path.join(__dirname, '..', 'extension', 'background.js'),
  'utf8'
);

function loadBackground({ sendMessageError = false } = {}) {
  const calls = [];
  const listeners = {};
  const runtime = {
    lastError: null,
    connectNative() {
      return {
        postMessage() {},
        onMessage: { addListener() {} },
        onDisconnect: { addListener() {} }
      };
    },
    onMessage: { addListener(listener) { listeners.runtimeMessage = listener; } },
    onStartup: { addListener(listener) { listeners.startup = listener; } }
  };
  const chrome = {
    runtime,
    action: { onClicked: { addListener(listener) { listeners.action = listener; } } },
    alarms: {
      async get() { return { name: 'existing' }; },
      async create() {},
      onAlarm: { addListener(listener) { listeners.alarm = listener; } }
    },
    idle: { onStateChanged: { addListener(listener) { listeners.idle = listener; } } },
    scripting: { async executeScript() { return []; } },
    storage: {
      local: {
        async get() { return {}; },
        set(_value, callback) { if (callback) callback(); }
      }
    },
    tabs: {
      query(_query, callback) { callback([]); },
      update(tabId, update, callback) {
        calls.push(['update', tabId, update]);
        callback({ id: tabId, discarded: false, frozen: false });
      },
      reload(tabId, callback) {
        calls.push(['reload', tabId]);
        if (callback) callback();
      },
      sendMessage(tabId, message, callback) {
        calls.push(['sendMessage', tabId, message]);
        if (sendMessageError) runtime.lastError = { message: 'Receiving end does not exist' };
        if (callback) callback();
        runtime.lastError = null;
      },
      onUpdated: { addListener(listener) { listeners.updated = listener; } }
    }
  };
  const context = vm.createContext({
    chrome,
    URL,
    Promise,
    setTimeout: () => 0,
    clearTimeout() {}
  });
  vm.runInContext(backgroundSource, context, { filename: 'background.js' });
  return { context, calls };
}

function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

test('b.ai refresh prevents discard and reloads a frozen tab', () => {
  const { context, calls } = loadBackground();
  context.refreshBaiTab({
    id: 17,
    url: 'https://chat.b.ai/usage',
    discarded: false,
    frozen: true
  });

  assert.deepEqual(plain(calls), [
    ['update', 17, { autoDiscardable: false }],
    ['reload', 17]
  ]);
});

test('b.ai refresh reloads when the content script cannot receive the refresh message', () => {
  const { context, calls } = loadBackground({ sendMessageError: true });
  context.refreshBaiTab({
    id: 23,
    url: 'https://chat.b.ai/usage',
    discarded: false,
    frozen: false
  });

  assert.deepEqual(plain(calls), [
    ['update', 23, { autoDiscardable: false }],
    ['sendMessage', 23, { type: 'codexbar:bai:usage:refresh' }],
    ['reload', 23]
  ]);
});

test('b.ai refresh keeps an awake tab in place', () => {
  const { context, calls } = loadBackground();
  context.refreshBaiTab({
    id: 29,
    url: 'https://chat.b.ai/usage',
    discarded: false,
    frozen: false
  });

  assert.deepEqual(plain(calls), [
    ['update', 29, { autoDiscardable: false }],
    ['sendMessage', 29, { type: 'codexbar:bai:usage:refresh' }]
  ]);
});
