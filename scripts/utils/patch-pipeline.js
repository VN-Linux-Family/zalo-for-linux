'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { Worker } = require('worker_threads');
const { parseAst, applyEdits } = require('./ast-transform');
const logger = require('./logger');

const DEFAULT_MAX_WORKERS = 3;
const MIN_MEMORY_PER_WORKER_MB = 768;

function getAvailableMemoryBytes() {
  if (process.platform === 'linux') {
    try {
      const meminfo = fs.readFileSync('/proc/meminfo', 'utf8');
      const match = meminfo.match(/^MemAvailable:\s+(\d+)\s+kB$/m);
      if (match) return Number(match[1]) * 1024;
    } catch (_) {}
  }
  return os.freemem();
}

/**
 * Runs an ordered list of transforms against a single target file, reading once,
 * parsing AST at most once for batched `astSteps`, and writing once if modified.
 *
 * Supports both:
 *  - `{ astSteps, contentSteps }` (single-parse batching via `applyEdits`)
 *  - `{ steps }` (legacy per-step `{ needsAst, transform }` list)
 *
 * @param {object} options
 * @param {string} options.targetPath
 * @param {Array<{ name: string, collectEdits: Function }>} [options.astSteps]
 * @param {Array<{ name: string, transform: Function }>} [options.contentSteps]
 * @param {Array<{ name: string, needsAst?: boolean, transform: Function }>} [options.steps]
 * @returns {{ changed: boolean, applied: string[], parseCount: number }}
 */
function runFileGroup({ targetPath, astSteps = [], contentSteps = [], steps = [] }) {
  if (!fs.existsSync(targetPath)) {
    throw new Error(`Target file not found: ${targetPath}`);
  }

  let content = fs.readFileSync(targetPath, 'utf8');
  const initialContent = content;
  let root = null;
  let astDirty = false;
  let parseCount = 0;
  const applied = [];

  if (astSteps.length > 0) {
    const allAlreadyPatched = astSteps.every(
      step => typeof step.isAlreadyPatched === 'function' && step.isAlreadyPatched(content)
    );
    if (!allAlreadyPatched) {
      root = parseAst(content);
      parseCount++;
      const allEdits = [];
      for (const step of astSteps) {
        if (typeof step.isAlreadyPatched === 'function' && step.isAlreadyPatched(content)) {
          continue;
        }
        const res = step.collectEdits({ content, root, targetPath });
        if (res && Array.isArray(res.edits) && res.edits.length > 0) {
          allEdits.push(...res.edits);
          applied.push(step.name);
        }
      }
      if (allEdits.length > 0) {
        content = applyEdits(root, allEdits);
        astDirty = true;
      }
    }
  }

  for (const step of contentSteps) {
    const res = step.transform({ content, targetPath });
    if (res && typeof res.content === 'string' && res.content !== content) {
      content = res.content;
      astDirty = true;
      applied.push(step.name);
    }
  }

  for (const step of steps) {
    if (step.needsAst && (root === null || astDirty)) {
      root = parseAst(content);
      parseCount++;
      astDirty = false;
    }

    const result = step.transform({ content, root, targetPath });
    if (result && typeof result.content === 'string' && result.content !== content) {
      content = result.content;
      astDirty = true;
      applied.push(step.name);
    }
  }

  const changed = content !== initialContent;
  if (changed) {
    fs.writeFileSync(targetPath, content, 'utf8');
  }
  return { changed, applied, parseCount };
}

/**
 * Computes worker concurrency bounded by CPU cores, explicit cap, and free RAM
 * to respect the Peak PSS <= 1.5 GiB ceiling on user machines.
 */
function computeConcurrencyLimit({ maxWorkers = DEFAULT_MAX_WORKERS, freeMemBytes = getAvailableMemoryBytes(), cpuCount = os.cpus().length } = {}) {
  const freeMemMb = Math.floor(freeMemBytes / (1024 * 1024));
  const byMemory = Math.max(1, Math.floor(freeMemMb / MIN_MEMORY_PER_WORKER_MB));
  return Math.max(1, Math.min(maxWorkers, cpuCount || 1, byMemory));
}

function runWorkerTask(task, activeWorkers = null) {
  const workerPath = path.join(__dirname, 'patch-worker.js');
  return new Promise((resolve, reject) => {
    const worker = new Worker(workerPath, { workerData: task });
    if (activeWorkers) activeWorkers.add(worker);
    let settled = false;

    const cleanup = () => {
      if (activeWorkers) activeWorkers.delete(worker);
      worker.terminate().catch(() => {});
    };

    worker.on('message', (msg) => {
      if (settled) return;
      settled = true;
      cleanup();
      if (msg && msg.ok) {
        resolve(msg);
      } else {
        const err = new Error(msg?.error || `Worker task "${task.label || task.name}" failed`);
        err.logs = msg?.logs || [];
        reject(err);
      }
    });

    worker.on('error', (err) => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(err);
    });

    worker.on('exit', (code) => {
      if (!settled) {
        settled = true;
        if (activeWorkers) activeWorkers.delete(worker);
        reject(new Error(`Worker "${task.label || task.name}" exited unexpectedly with code ${code}`));
      }
    });
  });
}

/**
 * Executes disjoint tasks in a bounded worker_threads pool with fail-fast semantics
 * and non-interleaved buffered logging.
 */
async function runBoundedPool(tasks, options = {}) {
  if (!Array.isArray(tasks) || tasks.length === 0) return [];

  const concurrency = Math.min(
    tasks.length,
    computeConcurrencyLimit({ maxWorkers: options.maxWorkers })
  );
  const verbose = Boolean(options.verbose);
  const onProgress = typeof options.onProgress === 'function' ? options.onProgress : null;
  const activeWorkers = new Set();
  const executor = typeof options.executor === 'function'
    ? options.executor
    : (task) => runWorkerTask(task, activeWorkers);

  const results = new Array(tasks.length);
  let nextIndex = 0;
  let completed = 0;
  let aborted = false;
  let firstError = null;

  async function terminateActiveWorkers() {
    const pending = [...activeWorkers];
    activeWorkers.clear();
    await Promise.all(pending.map((w) => w.terminate().catch(() => {})));
  }

  async function workerLoop() {
    while (!aborted) {
      const idx = nextIndex++;
      if (idx >= tasks.length) return;

      const task = tasks[idx];
      try {
        const res = await executor(task);
        if (aborted) return;

        results[idx] = res;
        completed++;

        if (verbose && Array.isArray(res?.logs)) {
          for (const line of res.logs) {
            process.stdout.write(line);
          }
        }
        if (onProgress) {
          onProgress(completed, tasks.length, task);
        } else if (!verbose) {
          logger.progress(completed, tasks.length, task.label || task.name || `group-${idx + 1}`);
        }
      } catch (err) {
        if (!aborted) {
          aborted = true;
          firstError = err;
          if (Array.isArray(err.logs)) {
            for (const line of err.logs) {
              process.stderr.write(line);
            }
          }
          await terminateActiveWorkers();
        }
        throw firstError || err;
      }
    }
  }

  const workers = Array.from({ length: concurrency }, () => workerLoop());
  try {
    await Promise.all(workers);
  } catch (err) {
    throw firstError || err;
  }
  return results;
}

module.exports = {
  DEFAULT_MAX_WORKERS,
  MIN_MEMORY_PER_WORKER_MB,
  runFileGroup,
  computeConcurrencyLimit,
  runBoundedPool,
};
