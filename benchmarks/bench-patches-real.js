'use strict';

const path = require('path');
const fs = require('fs');
const { spawn } = require('child_process');

const MiB = 1024 * 1024;
const toMiB = (bytes) => `${(bytes / MiB).toFixed(2)} MiB`;

function readProcessTree(rootPid) {
  const pending = [rootPid];
  const visited = new Set();
  const total = { rss: 0, pss: 0, processes: 0 };

  while (pending.length) {
    const pid = pending.pop();
    if (visited.has(pid)) continue;
    visited.add(pid);

    try {
      total.processes++;

      const memory = fs.readFileSync(`/proc/${pid}/smaps_rollup`, 'utf8');
      const rss = memory.match(/^Rss:\s+(\d+)\s+kB$/m);
      const pss = memory.match(/^Pss:\s+(\d+)\s+kB$/m);
      total.rss += (Number(rss?.[1]) || 0) * 1024;
      total.pss += (Number(pss?.[1]) || 0) * 1024;

      const children = fs.readFileSync(`/proc/${pid}/task/${pid}/children`, 'utf8').trim();
      if (children) pending.push(...children.split(/\s+/).map(Number));
    } catch {
      // A process may exit between reading its PID and /proc files.
    }
  }

  return total;
}

function maxMemory(a, b) {
  return {
    rss: Math.max(a.rss, b.rss),
    pss: Math.max(a.pss, b.pss),
    processes: Math.max(a.processes, b.processes),
  };
}

function runPatch(fullPath) {
  return new Promise((resolve) => {
    const runner = [
      'console.log = () => {};',
      'Promise.resolve().then(() => require(process.argv[1]).main()).catch((error) => { console.error(error); process.exitCode = 1; });',
    ].join(' ');
    const child = spawn(process.execPath, ['-e', runner, fullPath], {
      cwd: process.cwd(),
      env: process.env,
      stdio: ['ignore', 'ignore', 'pipe'],
    });

    let stderr = '';
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => { stderr += chunk; });

    const start = performance.now();
    let peakMemory = { rss: 0, pss: 0, processes: 0 };
    let latestTree = { rss: 0, pss: 0, processes: 0 };
    const sample = () => {
      latestTree = readProcessTree(child.pid);
      peakMemory = maxMemory(peakMemory, latestTree);
    };

    const sampler = setInterval(sample, 10);
    sample();

    child.on('error', (error) => {
      clearInterval(sampler);
      resolve({ elapsedMs: performance.now() - start, peakMemory, error });
    });

    child.on('close', (code, signal) => {
      sample();
      clearInterval(sampler);
      const elapsedMs = performance.now() - start;
      resolve({
        elapsedMs,
        peakMemory,
        error: code === 0 ? null : new Error(stderr.trim() || `Child exited with ${signal || `code ${code}`}`),
      });
    });
  });
}

async function benchmarkRealPatches() {
  if (process.platform !== 'linux' || !fs.existsSync('/proc/self/smaps_rollup')) {
    throw new Error('This benchmark requires Linux /proc with smaps_rollup support.');
  }

  console.log('='.repeat(104));
  console.log(`BENCHMARK: Real patch execution | Runtime: Node.js ${process.version}`);
  console.log('Sampling: 10 ms | RAM includes each patch process and its child-process tree.');
  console.log('RSS is summed per process; PSS accounts for shared pages and is the better aggregate RAM estimate.');
  console.log('='.repeat(104));

  const patchesDir = path.join(__dirname, '..', 'scripts', 'patches');
  const patchFiles = fs.readdirSync(patchesDir).filter((file) => file.endsWith('.js')).sort();
  const results = [];
  let totalTime = 0;
  let suitePeak = { rss: 0, pss: 0, processes: 0 };

  for (const file of patchFiles) {
    const fullPath = path.join(patchesDir, file);
    const result = await runPatch(fullPath);
    results.push({ name: file, ...result });
    totalTime += result.elapsedMs;
    suitePeak = maxMemory(suitePeak, result.peakMemory);
  }

  results.sort((a, b) => b.elapsedMs - a.elapsedMs);
  console.log(`| ${'Patch'.padEnd(34)} | ${'Time'.padStart(10)} | ${'Peak RSS'.padStart(12)} | ${'Peak PSS'.padStart(12)} | ${'Processes'.padStart(9)} | ${'Result'.padStart(8)} |`);
  console.log('-'.repeat(104));

  for (const result of results) {
    console.log(
      `| ${result.name.padEnd(34)} | ${`${result.elapsedMs.toFixed(1)} ms`.padStart(10)} | ${toMiB(result.peakMemory.rss).padStart(12)} | ${toMiB(result.peakMemory.pss).padStart(12)} | ${String(result.peakMemory.processes).padStart(9)} | ${(result.error ? 'FAILED' : 'OK').padStart(8)} |`
    );
    if (result.error) console.error(`  ${result.name}: ${result.error.message}`);
  }

  console.log('-'.repeat(104));
  const failed = results.filter((result) => result.error).length;
  console.log(`Total wall time: ${totalTime.toFixed(2)} ms`);
  console.log(`Highest per-patch peak: RSS ${toMiB(suitePeak.rss)} | PSS ${toMiB(suitePeak.pss)} | processes ${suitePeak.processes}`);
  console.log(`Patches: ${results.length} | Failed: ${failed}`);
  console.log('='.repeat(104));

  if (failed) process.exitCode = 1;
}

benchmarkRealPatches().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
