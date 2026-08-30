const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const APP_DIR = path.join(__dirname, '..', '..', 'app');
const MAIN_DIST_DIR = path.join(APP_DIR, 'main-dist');
const AUTO_LAUNCH_BUNDLES = ['main.js', 'compact-app.js'];

// main-dist/main.js 26.08.20+ release: launcher=d, initializer=u, config=l (swap d and l).

// const LAUNCHER_OPTIONS_ORIGINAL =
//   'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}l=new r(e)';
// const LAUNCHER_OPTIONS_PATCHED =
//   'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}else if("linux"===process.platform)e.path=process.env.APPIMAGE||i.getPath("exe");l=new r(e)';
const LAUNCHER_OPTIONS_ORIGINAL =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}d=new r(e)';
const LAUNCHER_OPTIONS_PATCHED =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}else if("linux"===process.platform)e.path=process.env.APPIMAGE||i.getPath("exe");d=new r(e)';
const COMPACT_LAUNCHER_OPTIONS_ORIGINAL =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}u=new r(e)';
const COMPACT_LAUNCHER_OPTIONS_PATCHED =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}else if("linux"===process.platform)e.path=process.env.APPIMAGE||i.getPath("exe");u=new r(e)';

// const GET_LAUNCHER_ORIGINAL = 'getZaloLauncher:()=>l';
// const GET_LAUNCHER_PATCHED = 'getZaloLauncher:()=>{if(!l)u(d);return l}';
const GET_LAUNCHER_ORIGINAL = 'getZaloLauncher:()=>d';
const GET_LAUNCHER_PATCHED = 'getZaloLauncher:()=>{if(!d)u(l);return d}';

const COMPACT_GET_LAUNCHER_ORIGINAL = 'getZaloLauncher:()=>u';
const COMPACT_GET_LAUNCHER_PATCHED = 'getZaloLauncher:()=>{if(!u)d(l);return u}';

const HANDLERS_ORIGINAL = new RegExp(
  'checkAutoLaunchEnable:e=>\\{const\\{getZaloLauncher:t\\}=n\\("([A-Za-z0-9._-]+)"\\);return t\\(\\)\\.isEnabled\\(\\)\\},' +
  'toggleAutoLaunch:\\(e,t\\)=>\\{const\\{appConfig:r\\}=n\\("([A-Za-z0-9._-]+)"\\),\\{getZaloLauncher:i\\}=n\\("\\1"\\),o=i\\(\\);' +
  't\\?\\(([A-Za-z_$][A-Za-z0-9_$]*)\\.zsymb\\(4,"([A-Za-z0-9._-]+)",\\["autolaunch to enable","([A-Za-z0-9._-]+)"\\]\\),o\\.enable\\(\\),r\\.set\\("autolaunch",!0\\)\\):' +
  '\\(\\3\\.zsymb\\(4,"([A-Za-z0-9._-]+)",\\["autolaunch to disable","([A-Za-z0-9._-]+)"\\]\\),o\\.disable\\(\\),r\\.set\\("autolaunch",!1\\)\\)\\},'
);

const HANDLERS_PATCHED =
  'checkAutoLaunchEnable:async e=>{try{const{getZaloLauncher:t}=n("$1"),r=t();' +
  'return!!(r&&"function"==typeof r.isEnabled)&&!!await r.isEnabled()}catch(e){return!1}},' +
  'toggleAutoLaunch:async(e,t)=>{const{appConfig:r}=n("$2"),{getZaloLauncher:i}=n("$1");try{' +
  'const o=i();if(!o)return r.set("autolaunch",!!t),!1;return t?' +
  '($3.zsymb(4,"$4",["autolaunch to enable","$5"]),await o.enable(),r.set("autolaunch",!0),!0):' +
  '($3.zsymb(4,"$6",["autolaunch to disable","$7"]),await o.disable(),r.set("autolaunch",!1),!0)' +
  '}catch(e){return $3.zsymb(19,"linux_auto_launch_error",e),!1}},';

function replaceRequired(content, original, replacement, label) {
  if (content.includes(replacement) || content.includes('linux_auto_launch_error')) {
    return content;
  }

  if (original instanceof RegExp) {
    if (original.test(content)) {
      return content.replace(original, replacement);
    }
  } else if (content.includes(original)) {
    return content.replace(original, replacement);
  }
  throw new Error(`Auto-launch ${label} anchor not found; upstream Zalo bundle may have changed.`);
}

function patchAutoLaunch(content) {
  const isMainVariant = content.includes(LAUNCHER_OPTIONS_ORIGINAL) ||
    content.includes(LAUNCHER_OPTIONS_PATCHED) || content.includes(GET_LAUNCHER_PATCHED);
  const isCompactVariant = content.includes(COMPACT_LAUNCHER_OPTIONS_ORIGINAL) ||
    content.includes(COMPACT_LAUNCHER_OPTIONS_PATCHED) || content.includes(COMPACT_GET_LAUNCHER_PATCHED);
  if (!isMainVariant && !isCompactVariant) {
    throw new Error('Auto-launch launcher module anchor not found; upstream Zalo bundle may have changed.');
  }

  if (isMainVariant) {
    content = replaceRequired(
      content, LAUNCHER_OPTIONS_ORIGINAL, LAUNCHER_OPTIONS_PATCHED, 'Linux path'
    );
    content = replaceRequired(
      content, GET_LAUNCHER_ORIGINAL, GET_LAUNCHER_PATCHED, 'lazy initialization'
    );
  } else {
    content = replaceRequired(
      content, COMPACT_LAUNCHER_OPTIONS_ORIGINAL, COMPACT_LAUNCHER_OPTIONS_PATCHED, 'Linux path'
    );
    content = replaceRequired(
      content, COMPACT_GET_LAUNCHER_ORIGINAL, COMPACT_GET_LAUNCHER_PATCHED, 'lazy initialization'
    );
  }
  content = replaceRequired(
    content, HANDLERS_ORIGINAL, HANDLERS_PATCHED, 'IPC handler'
  );
  return content;
}

async function main() {
  logger.info('Patching auto-launch for Linux...');
  let checkedCount = 0;
  let updatedCount = 0;

  for (const fileName of AUTO_LAUNCH_BUNDLES) {
    const filePath = path.join(MAIN_DIST_DIR, fileName);
    if (!fs.existsSync(filePath)) {
      if (fileName === 'main.js') throw new Error('main-dist/main.js not found');
      logger.warn(`Skipping ${fileName} (not found)`);
      continue;
    }
    const original = fs.readFileSync(filePath, 'utf8');
    const patched = patchAutoLaunch(original);
    checkedCount += 1;
    if (patched !== original) {
      fs.writeFileSync(filePath, patched, 'utf8');
      updatedCount += 1;
      logger.dim(`Patched auto-launch handlers in ${fileName}`);
    }
  }
  logger.success(`Linux auto-launch patch applied (${checkedCount} checked, ${updatedCount} updated)`);
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('Auto-launch patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, patchAutoLaunch };
