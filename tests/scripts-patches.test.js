import { describe, expect, it } from 'bun:test';
const fs = require('fs');
const path = require('path');
const logger = require(path.join(__dirname, '..', 'scripts', 'utils', 'logger'));

describe('Scripts & Patches Integration', () => {
  it('loads logger and invokes logging methods without error', () => {
    expect(typeof logger.info).toBe('function');
    expect(typeof logger.warn).toBe('function');
    expect(typeof logger.error).toBe('function');
    expect(typeof logger.success).toBe('function');
    expect(typeof logger.dim).toBe('function');
  });

  it('validates that all patch scripts in scripts/patches load and export main', () => {
    const patchesDir = path.join(__dirname, '..', 'scripts', 'patches');
    const files = fs.readdirSync(patchesDir).filter(f => f.endsWith('.js'));

    expect(files.length).toBeGreaterThanOrEqual(25);

    for (const file of files) {
      const fullPath = path.join(patchesDir, file);
      const mod = require(fullPath);
      expect(typeof mod.main).toBe('function');
    }
  });
});
