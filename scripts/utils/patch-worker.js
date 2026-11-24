'use strict';

const path = require('path');
const { parentPort, workerData } = require('worker_threads');

async function executeTask(task) {
  const logs = [];
  const origLog = console.log;
  const origWarn = console.warn;
  const origError = console.error;

  const capture = (prefix = '') => (...args) => {
    const line = args.map(a => (typeof a === 'string' ? a : String(a))).join(' ');
    logs.push(`${prefix}${line}\n`);
  };

  console.log = capture();
  console.warn = capture();
  console.error = capture();

  const start = performance.now();
  try {
    if (task.type === 'fn') {
      const mod = require(path.resolve(__dirname, task.module));
      await Promise.resolve(mod[task.exportName](...(task.args || [])));
    } else {
      const modPath = path.join(__dirname, '..', task.task ? `${task.name}.js` : `patches/${task.name}.js`);
      const mod = require(modPath);
      await Promise.resolve(mod.main(...(task.args || [])));
    }
    return { ok: true, logs, durationMs: performance.now() - start };
  } catch (err) {
    return {
      ok: false,
      error: err && err.stack ? err.stack : String(err),
      logs,
      durationMs: performance.now() - start,
    };
  } finally {
    console.log = origLog;
    console.warn = origWarn;
    console.error = origError;
  }
}

if (parentPort && workerData) {
  executeTask(workerData).then((result) => {
    parentPort.postMessage(result);
  });
}

module.exports = { executeTask };
