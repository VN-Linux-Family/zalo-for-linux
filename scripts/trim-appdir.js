#!/usr/bin/env node
'use strict';

/**
 * trim-appdir.js
 *
 * Trims the Electron runtime payload of an extracted AppDir before it is
 * repacked. Runs from build-stage2.sh for both the fresh build and the reused
 * AppDir of the ZaDark phase, and is idempotent.
 *
 * Always removes Electron locale packs that the application will never use
 * (every locale falls back to en-US when its .pak is missing). The optional
 * GPU fallbacks are opt-in because VMs and remote desktops may rely on them.
 *
 * Environment:
 *   ZALO_KEEP_LOCALES=1       keep every locale pack (skip the default trim)
 *   ZALO_TRIM_SWIFTSHADER=1   drop libvk_swiftshader (software Vulkan fallback)
 *   ZALO_TRIM_VULKAN=1        drop libvulkan.so.1 (Vulkan loader)
 */

const fs = require('fs-extra');
const path = require('path');
const logger = require('./utils/logger');

const { DIST_DIR } = require('./utils/paths');

// Zalo ships Vietnamese and English; other locales fall back to en-US.
const DEFAULT_LOCALES = ['vi', 'en-US', 'en-GB'];

const SWIFTSHADER_FILES = ['libvk_swiftshader.so', 'libvk_swiftshader_icd.json'];
const VULKAN_FILES = ['libvulkan.so.1'];

function envEnabled(name) {
  const value = process.env[name];
  return value === '1' || value === 'true';
}

async function sizeOf(file) {
  try {
    return (await fs.stat(file)).size;
  } catch (_) {
    return 0;
  }
}

function asMb(bytes) {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

async function trimLocales(appDir) {
  if (envEnabled('ZALO_KEEP_LOCALES')) {
    logger.dim('Locale trim disabled by ZALO_KEEP_LOCALES');
    return 0;
  }

  const localesDir = path.join(appDir, 'locales');
  if (!(await fs.pathExists(localesDir))) {
    logger.warn(`No locales directory in ${appDir}; skipping locale trim`);
    return 0;
  }

  let removed = 0;
  let bytes = 0;
  for (const entry of await fs.readdir(localesDir)) {
    if (!entry.endsWith('.pak')) continue;
    if (DEFAULT_LOCALES.includes(entry.slice(0, -'.pak'.length))) continue;

    const full = path.join(localesDir, entry);
    bytes += await sizeOf(full);
    await fs.remove(full);
    removed++;
  }

  logger.dim(
    `Removed ${removed} unused locale pack(s), kept ${DEFAULT_LOCALES.join(', ')} (${asMb(bytes)})`
  );
  return bytes;
}

async function trimFiles(appDir, files, label) {
  let removed = 0;
  let bytes = 0;

  for (const name of files) {
    const full = path.join(appDir, name);
    if (!(await fs.pathExists(full))) continue;

    bytes += await sizeOf(full);
    await fs.remove(full);
    removed++;
  }

  if (removed) logger.dim(`Removed ${label} (${asMb(bytes)})`);
  return bytes;
}

/**
 * @param {string} [appDir] Extracted AppDir; defaults to $APPDIR or
 *   dist/squashfs-root so the script is also runnable on its own.
 * @returns {Promise<{ removedBytes: number }>}
 */
async function main(appDir) {
  const dir = path.resolve(appDir || process.env.APPDIR || path.join(DIST_DIR, 'squashfs-root'));

  if (!(await fs.pathExists(dir))) {
    logger.warn(`AppDir not found, skipping trim: ${dir}`);
    return { removedBytes: 0 };
  }

  logger.info(`Trimming Electron runtime payload: ${dir}`);

  let removedBytes = await trimLocales(dir);

  if (envEnabled('ZALO_TRIM_SWIFTSHADER')) {
    removedBytes += await trimFiles(dir, SWIFTSHADER_FILES, 'SwiftShader fallback');
  }
  if (envEnabled('ZALO_TRIM_VULKAN')) {
    removedBytes += await trimFiles(dir, VULKAN_FILES, 'Vulkan loader');
  }

  logger.success(`Trimmed ${asMb(removedBytes)} from the AppImage payload`);
  return { removedBytes };
}

if (require.main === module) {
  main(process.argv[2]).catch((err) => {
    logger.error('trim-appdir failed:', err.message);
    process.exit(1);
  });
}

module.exports = { main, DEFAULT_LOCALES, trimLocales };
