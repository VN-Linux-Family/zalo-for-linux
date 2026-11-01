'use strict';

const path = require('path');
const fs = require('fs');

async function benchmarkRealPatches() {
  console.log('='.repeat(80));
  console.log(`BENCHMARK: 28 Patches Execution on Real Extracted App (4.2MB+ bundle)`);
  console.log(`Runtime: ${typeof Bun !== 'undefined' ? `Bun v${Bun.version}` : `Node.js ${process.version}`}`);
  console.log('='.repeat(80));

  const patchesDir = path.join(__dirname, '..', 'scripts', 'patches');
  const patchFiles = fs.readdirSync(patchesDir).filter(f => f.endsWith('.js'));

  const results = [];
  let totalTime = 0;

  for (const file of patchFiles) {
    const fullPath = path.join(patchesDir, file);
    const mod = require(fullPath);
    if (typeof mod.main !== 'function') continue;

    // Suppress console.log during patch benchmark to measure pure regex/disk I/O
    const originalLog = console.log;
    console.log = () => { };

    const start = performance.now();
    try {
      await mod.main();
    } catch (e) {
      // ignore errors if file already patched
    }
    const elapsed = performance.now() - start;
    console.log = originalLog;

    totalTime += elapsed;
    results.push({ name: file, elapsed });
  }

  // Sort slowest first
  results.sort((a, b) => b.elapsed - a.elapsed);

  console.log(`| ${'Patch File'.padEnd(36)} | ${'Time (ms)'.padStart(12)} | ${'Share (%)'.padStart(12)} |`);
  console.log('-'.repeat(80));
  for (const r of results) {
    const pct = ((r.elapsed / totalTime) * 100).toFixed(1);
    console.log(`| ${r.name.padEnd(36)} | ${r.elapsed.toFixed(2).padStart(10)} ms | ${(pct + ' %').padStart(12)} |`);
  }
  console.log('-'.repeat(80));
  console.log(`| ${'TOTAL RUNTIME'.padEnd(36)} | ${totalTime.toFixed(2).padStart(10)} ms | ${'100.0 %'.padStart(12)} |`);
  console.log('='.repeat(80));
}

benchmarkRealPatches();
