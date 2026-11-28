'use strict';

const fs = require('fs');
const path = require('path');
const { APP_MAIN_DIST, APP_PC_DIST, resolveLazyLoginBundles } = require('./paths');
const { runFileGroup } = require('./patch-pipeline');

const patchTitlebarLinux = require('../patches/patch-titlebar-linux');
const patchLinuxGuards = require('../patches/patch-linux-guards');
const patchQuitNoShow = require('../patches/patch-quit-no-show');
const patchShellOpenLinux = require('../patches/patch-shell-open-linux');
const patchAutoTheme = require('../patches/patch-auto-theme');
const patchAutoLaunch = require('../patches/patch-auto-launch');
const patchCallgate = require('../patches/patch-zcall-callgate');
const patchLanguageBridge = require('../patches/patch-zalo-language-bridge');
const patchDiskSpace = require('../patches/patch-disk-space');
const patchZocrPcDist = require('../patches/patch-zocr-pc-dist');
const { patchXdgUserDirs } = require('../patches/patch-xdg-user-dirs');
const patchAppLockTitlebar = require('../patches/patch-app-lock-titlebar');

const PC_DIST_TARGETS = {
  'compact-app-pc': {
    pattern: path.join(APP_PC_DIST, 'compact-app-pc.*.js'),
    withLanguageBridge: true,
    requireCallgate: true,
  },
  'lazy-login': {
    resolve: () => resolveLazyLoginBundles(APP_PC_DIST)[0] || null,
    pattern: path.join(APP_PC_DIST, 'lazy', 'default-login-main-startup-shared-worker.*.js'),
    withLanguageBridge: true,
    requireCallgate: true,
  },
  'search-worker': {
    pattern: path.join(APP_PC_DIST, 'search-worker.*.js'),
    withLanguageBridge: false,
    requireCallgate: false,
  },
  'sync-v2-sub-worker': {
    pattern: path.join(APP_PC_DIST, 'sync-v2-sub-worker.*.js'),
    withLanguageBridge: false,
    requireCallgate: false,
  },
};

function runPcDistBundleGroup(bundleKey) {
  const spec = PC_DIST_TARGETS[bundleKey];
  if (!spec) {
    throw new Error(`Unknown pc-dist bundle key: ${bundleKey}`);
  }
  const targetPath = spec.resolve ? spec.resolve() : patchCallgate.resolveGlobTarget(spec.pattern);
  if (!targetPath) {
    throw new Error(`pc-dist target not found for ${bundleKey}: ${spec.pattern}`);
  }

  const contentSteps = [];
  if (spec.withLanguageBridge) {
    contentSteps.push({
      name: 'patch-zalo-language-bridge',
      transform: ({ content }) => {
        if (content.includes(patchLanguageBridge.EVENT_BRIDGE)) return { content };
        const res = patchLanguageBridge.patchContent(content);
        return { content: res.content };
      },
    });
  }
  contentSteps.push(
    {
      name: 'patch-zcall-callgate',
      transform: ({ content }) => {
        if (content.includes('"linux"!==$znode.os.platform()') || content.includes('"linux" !== $znode.os.platform()')) {
          return { content };
        }
        const snippetRes = patchCallgate.patchCallgateViaSnippetAst(content);
        if (snippetRes.editCount > 0) return { content: snippetRes.content };
        const { parseAst, applyEdits } = require('./ast-transform');
        try {
          const root = parseAst(content);
          const { edits } = patchCallgate.collectCallgateEdits(content, root);
          if (edits.length > 0) return { content: applyEdits(root, edits) };
        } catch (_) {}
        if (!spec.requireCallgate && !content.includes('split(".")[0])<17') && !content.includes(patchCallgate.ORIGINAL)) {
          return { content };
        }
        if (!content.includes(patchCallgate.ORIGINAL)) {
          throw new Error(`callgate patch not applied: ${path.basename(targetPath)}: kernel gate`);
        }
        let updated = content.split(patchCallgate.ORIGINAL).join(patchCallgate.PATCHED);
        if (updated.includes(patchCallgate.DEFAULTS_ORIGINAL)) {
          updated = updated.split(patchCallgate.DEFAULTS_ORIGINAL).join(patchCallgate.DEFAULTS_PATCHED);
        }
        return { content: updated };
      },
    },
    {
      name: 'patch-disk-space',
      transform: ({ content }) => ({ content: patchDiskSpace.patchDiskSpace(content) }),
    },
    {
      name: 'patch-zocr-pc-dist',
      transform: ({ content }) => {
        if (!content.includes('ocr-text-overlay')) return { content };
        return { content: patchZocrPcDist.patch(content) };
      },
    },
    {
      name: 'patch-xdg-user-dirs',
      transform: ({ content }) => {
        if (!content.includes('getDefaultZaloReceivedFiles') || !content.includes('createAppPathManager')) {
          return { content };
        }
        return { content: patchXdgUserDirs(content) };
      },
    },
  );

  if (bundleKey === 'compact-app-pc' || bundleKey === 'lazy-login') {
    contentSteps.push({
      name: 'patch-app-lock-titlebar',
      transform: ({ content }) => ({ content: patchAppLockTitlebar.patchContent(content).content }),
    });
  }

  return runFileGroup({ targetPath, contentSteps });
}

async function runMainDistGroup() {
  const mainJsPath = path.join(APP_MAIN_DIST, 'main.js');
  if (fs.existsSync(mainJsPath)) {
    runFileGroup({
      targetPath: mainJsPath,
      astSteps: [
        {
          name: 'patch-titlebar-linux',
          collectEdits: ({ content, root }) => patchTitlebarLinux.collectEdits(content, root),
        },
        {
          name: 'patch-linux-guards',
          isAlreadyPatched: (content) => content.includes(patchLinuxGuards.PLATFORM_GUARD),
          collectEdits: ({ content, root }) => patchLinuxGuards.collectEdits(content, root),
        },
        {
          name: 'patch-quit-no-show',
          isAlreadyPatched: (content) => content.includes('||"linux"===process.platform||this.mainWindow.show()'),
          collectEdits: ({ content, root }) => patchQuitNoShow.collectEdits(content, root),
        },
        {
          name: 'patch-shell-open-linux',
          isAlreadyPatched: (content) =>
            /if\("linux"===process\.platform&&([A-Za-z_$][\w$]*)\.isDirectory\(\)\)return [A-Za-z_$][\w$]*\(([A-Za-z_$][\w$]*)\)/.test(content),
          collectEdits: ({ content, root }) => patchShellOpenLinux.collectEdits(content, root),
        },
        {
          name: 'patch-auto-theme',
          isAlreadyPatched: (content) => content.includes('zalo-linux-theme-change'),
          collectEdits: ({ content, root }) => patchAutoTheme.collectMainEdits(content, root),
        },
      ],
      contentSteps: [
        {
          name: 'patch-titlebar-content',
          transform: ({ content }) => ({ content: patchTitlebarLinux.patchTitlebarContent(content) }),
        },
        {
          name: 'patch-quit-no-show-fallback',
          transform: ({ content }) => {
            if (content.includes('||"linux"===process.platform||this.mainWindow.show()')) return { content };
            return { content: patchQuitNoShow.patchQuitContent(content) };
          },
        },
        {
          name: 'patch-shell-open-linux-fallback',
          transform: ({ content }) => {
            if (/if\("linux"===process\.platform&&([A-Za-z_$][\w$]*)\.isDirectory\(\)\)return [A-Za-z_$][\w$]*\(([A-Za-z_$][\w$]*)\)/.test(content)) {
              return { content };
            }
            return { content: patchShellOpenLinux.patchShellOpenLinux(content) };
          },
        },
      ],
    });
    patchAutoLaunch.patchFile(mainJsPath);
  }

  patchAutoTheme.patchPreload(APP_MAIN_DIST);

  // Secondary/non-AST main-dist patches (compact-app.js runs in parallel in runCompactAppGroup;
  // patch-flatpak-file-transfer runs in Wave 2 so it never races compact-app.js)
  const remaining = [
    'patch-pasting-img',
    'patch-db-cross-v4',
    'patch-os-sys-info',
    'patch-zcall-native',
    'patch-notification-badge',
    'patch-zadark-keep',
  ];
  for (const name of remaining) {
    await Promise.resolve(require(`../patches/${name}`).main());
  }
}

function runCompactAppGroup() {
  const compactAppPath = path.join(APP_MAIN_DIST, 'compact-app.js');
  if (!fs.existsSync(compactAppPath)) return;
  patchAutoLaunch.patchFile(compactAppPath);
  patchQuitNoShow.patchFile(compactAppPath);
  patchShellOpenLinux.patchFile(compactAppPath);
}

module.exports = {
  PC_DIST_TARGETS,
  runPcDistBundleGroup,
  runMainDistGroup,
  runCompactAppGroup,
};
