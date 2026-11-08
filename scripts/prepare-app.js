'use strict';

const logger = require('./utils/logger');
const { main: extractMain } = require('./extract');
const { main: buildNativeMain } = require('./build-native');
const { main: applyPatchesMain } = require('./apply-patches');

async function main() {
  logger.step('STEP 1: Extracting Zalo Application Sources');
  await extractMain();

  logger.step('STEP 2: Building Native Addons & Engines');
  await buildNativeMain();

  logger.step('STEP 3: Applying Platform Patches');
  await applyPatchesMain();

  logger.success('Application preparation complete');
}

if (require.main === module) {
  main().catch((err) => {
    logger.error('Prepare-app failed:', err.message);
    process.exit(1);
  });
}

module.exports = { main };
