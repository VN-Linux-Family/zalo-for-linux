'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const applyPatches = require('../scripts/apply-patches');
const astTransform = require('../scripts/utils/ast-transform');
const {
  runFileGroup,
  computeConcurrencyLimit,
  runBoundedPool,
  DEFAULT_MAX_WORKERS,
} = require('../scripts/utils/patch-pipeline');

describe('Shared-File AST Patch Pipeline & Bounded Pool', () => {
  it('INV-1: partitions every patch in PATCHES into disjoint groups or cross-cutting wave without duplicates', () => {
    const scheduled = [];
    for (const entry of applyPatches.DISJOINT_GROUPS) {
      if (entry.type === 'group') {
        for (const item of entry.items) scheduled.push(item.name);
      } else if (entry.type === 'fn' && Array.isArray(entry.patches)) {
        scheduled.push(...entry.patches);
      } else if (entry.type === 'patch') {
        scheduled.push(entry.name);
      }
    }
    for (const name of applyPatches.CROSS_CUTTING_PATCHES) {
      scheduled.push(name);
    }

    assert.strictEqual(
      new Set(scheduled).size,
      scheduled.length,
      'Duplicate patch found across DISJOINT_GROUPS / CROSS_CUTTING_PATCHES'
    );
    assert.deepStrictEqual(
      new Set(scheduled),
      new Set(applyPatches.PATCHES),
      'Scheduled patches must match PATCHES exactly'
    );
  });

  it('INV-2 & INV-3: runFileGroup parses AST once for batched astSteps and is idempotent', () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-pipeline-test-'));
    const targetPath = path.join(tmpDir, 'bundle.js');
    fs.writeFileSync(targetPath, 'const cfg = { enableCall: !1, enableVideoCall: !1 };', 'utf8');

    try {
      const astSteps = [
        {
          name: 'enable-call',
          isAlreadyPatched: (content) => content.includes('enableCall: !0'),
          collectEdits: ({ root }) => {
            const p = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableCall$' } } })
              .find(n => n.field('value')?.text() === '!1');
            return { edits: p ? [p.replace('enableCall: !0')] : [] };
          },
        },
        {
          name: 'enable-video',
          isAlreadyPatched: (content) => content.includes('enableVideoCall: !0'),
          collectEdits: ({ root }) => {
            const p = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableVideoCall$' } } })
              .find(n => n.field('value')?.text() === '!1');
            return { edits: p ? [p.replace('enableVideoCall: !0')] : [] };
          },
        },
      ];

      const first = runFileGroup({ targetPath, astSteps });
      assert.strictEqual(first.changed, true);
      assert.strictEqual(first.parseCount, 1);
      assert.deepStrictEqual(first.applied, ['enable-call', 'enable-video']);
      assert.strictEqual(
        fs.readFileSync(targetPath, 'utf8'),
        'const cfg = { enableCall: !0, enableVideoCall: !0 };'
      );

      // Idempotent second run skips parseAst completely
      const second = runFileGroup({ targetPath, astSteps });
      assert.strictEqual(second.changed, false);
      assert.strictEqual(second.parseCount, 0);
      assert.deepStrictEqual(second.applied, []);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it('INV-4: computeConcurrencyLimit bounds workers by free RAM and maxWorkers cap', () => {
    assert.strictEqual(
      computeConcurrencyLimit({ maxWorkers: 3, freeMemBytes: 400 * 1024 * 1024, cpuCount: 16 }),
      1
    );
    assert.strictEqual(
      computeConcurrencyLimit({ maxWorkers: 3, freeMemBytes: 1600 * 1024 * 1024, cpuCount: 16 }),
      2
    );
    assert.strictEqual(
      computeConcurrencyLimit({ maxWorkers: DEFAULT_MAX_WORKERS, freeMemBytes: 8192 * 1024 * 1024, cpuCount: 16 }),
      DEFAULT_MAX_WORKERS
    );
  });

  it('INV-6: runBoundedPool aborts fail-fast when a task fails and tracks progress', async () => {
    const executed = [];
    const progressEvents = [];
    const tasks = [
      { name: 't1', label: 't1' },
      { name: 't2', label: 't2' },
      { name: 't3', label: 't3' },
      { name: 't4', label: 't4' },
    ];

    await assert.rejects(
      () =>
        runBoundedPool(tasks, {
          maxWorkers: 2,
          onProgress: (done, total, task) => progressEvents.push({ done, total, name: task.name }),
          executor: async (task) => {
            executed.push(task.name);
            if (task.name === 't2') {
              throw new Error('simulated patch failure');
            }
            return { ok: true, logs: [] };
          },
        }),
      /simulated patch failure/
    );

    assert.ok(executed.includes('t2'));
    assert.ok(progressEvents.length <= tasks.length);
  });

  it('Snippet AST extractor skips braces inside strings, templates, comments, and regex literals (including after keywords)', () => {
    const src = [
      'const a = "{"; const b = \'}\';',
      'const r = /\\{fake\\}/g;',
      'function f() { return /\\}/; }',
      'function div(a) { return a++/2; }',
      'const t = `hello ${"}" + `${1}` /* } */} {world}`;',
      'const m = new class { foo() { return "}{"; } changeToLang() { /* } */ return 1; } };',
    ].join('\n');

    const clsIdx = src.indexOf('new class {');
    const openIdx = src.indexOf('{', clsIdx);
    const block = astTransform.extractBlockFrom(src, openIdx, { includePrefixRe: /new\s+class\s*$/ });
    assert.ok(block, 'extractBlockFrom should find the class block');
    assert.ok(block.snippet.startsWith('new class {'));
    assert.ok(block.snippet.endsWith('}'));
    assert.ok(block.snippet.includes('changeToLang()'));

    const anchorIdx = src.indexOf('return 1;');
    const enclosed = astTransform.extractEnclosingBlock(src, anchorIdx, { levels: 1 });
    assert.ok(enclosed, 'extractEnclosingBlock should find method body');
    assert.ok(enclosed.snippet.includes('return 1;'));
  });

  it('INV-6 (real worker_threads): runBoundedPool fails fast and terminates active workers on worker error', async () => {
    const tasks = [
      { type: 'patch', name: '__non_existent_patch_for_test__', label: 'failing-worker' },
      { type: 'patch', name: 'patch-sqlite3', label: 'sqlite3' },
    ];
    await assert.rejects(
      () => runBoundedPool(tasks, { maxWorkers: 2, verbose: false }),
      /Cannot find module|failed/
    );
  });
});
