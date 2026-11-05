const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const APP_DIR = path.join(__dirname, '..', '..', 'app');
const MAIN_DIST_DIR = path.join(APP_DIR, 'main-dist');
const AUTO_LAUNCH_BUNDLES = ['main.js', 'compact-app.js'];

const START = '// --- Zalo Linux DBus Autostart ---';
const END = '// --- End Zalo Linux DBus Autostart ---';

const DBUS_LAUNCHER_HELPER = `function __zaloWrapLinuxLauncher(r){
  const{execFile:e}=require("child_process"),t=require("fs"),n=Boolean(process.env.FLATPAK_ID||t.existsSync("/.flatpak-info"));
  const APP_ID="net.vietnamlinuxfamily.zalo_for_linux";
  function o(t,n=3000){return new Promise(((o,a)=>{e("gdbus",t,{timeout:n},((e,t,n)=>e?a(new Error(n||(e&&e.message)||"gdbus error")):o(t?t.trim():"")))}))}
  return{
    async isEnabled(){
      if(n){
        try{
          const e=await o(["call","--session","--dest","org.freedesktop.impl.portal.PermissionStore","--object-path","/org/freedesktop/impl/portal/PermissionStore","--method","org.freedesktop.impl.portal.PermissionStore.GetPermission","background","background",APP_ID]);
          if(e.includes("'yes'"))return!0;
          if(e.includes("'no'"))return!1
        }catch(e){}
      }
      if(r&&"function"==typeof r.isEnabled)try{return await r.isEnabled()}catch(e){}
      return!1
    },
    async enable(){
      let e=!1;
      try{
        if(n)try{await o(["call","--session","--dest","org.freedesktop.impl.portal.PermissionStore","--object-path","/org/freedesktop/impl/portal/PermissionStore","--method","org.freedesktop.impl.portal.PermissionStore.SetPermission","background","true","background",APP_ID,"['yes']"],3000)}catch(_){}
        const t="zalo_auto_"+Math.floor(1e8*Math.random()),a=n?"['zalo', '--file-forwarding', '--hidden']":"['"+(process.env.APPIMAGE||process.execPath)+"', '--hidden']",s="{'handle_token': <'"+t+"'>, 'reason': <'Tự động khởi động Zalo khi đăng nhập'>, 'autostart': <true>, 'commandline': <"+a+">, 'dbus-activatable': <false>}";
        await o(["call","--session","--dest","org.freedesktop.portal.Desktop","--object-path","/org/freedesktop/portal/desktop","--method","org.freedesktop.portal.Background.RequestBackground","",s],5000);
        e=!0
      }catch(t){console.warn("[autostart-dbus] Portal enable failed:",t&&t.message||t)}
      if(r&&"function"==typeof r.enable)try{await r.enable()}catch(t){if(!e)throw t}
    },
    async disable(){
      try{
        const e="zalo_auto_"+Math.floor(1e8*Math.random()),t="{'handle_token': <'"+e+"'>, 'autostart': <false>}";
        await o(["call","--session","--dest","org.freedesktop.portal.Desktop","--object-path","/org/freedesktop/portal/desktop","--method","org.freedesktop.portal.Background.RequestBackground","",t],5000);
        if(n)try{await o(["call","--session","--dest","org.freedesktop.impl.portal.PermissionStore","--object-path","/org/freedesktop/impl/portal/PermissionStore","--method","org.freedesktop.impl.portal.PermissionStore.SetPermission","background","false","background",APP_ID,"['no']"],3000)}catch(_){}
      }catch(e){console.warn("[autostart-dbus] Portal disable failed:",e&&e.message||e)}
      if(r&&"function"==typeof r.disable)try{await r.disable()}catch(e){}
    }
  }
}`;

function injectHelper(content) {
  const block = `${START}\n${DBUS_LAUNCHER_HELPER}\n${END}\n`;
  const start = content.indexOf(START);
  if (start === -1) return block + content;
  const end = content.indexOf(END, start);
  if (end === -1) throw new Error('DBus autostart end marker not found');
  return content.slice(0, start) + block + content.slice(end + END.length).replace(/^\r?\n/, '');
}

// main-dist/main.js 26.08.20+ release: launcher=d, initializer=u, config=l (swap d and l).

const LAUNCHER_OPTIONS_ORIGINAL =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}d=new r(e)';
// On Linux, wrap launcher with DBus portal Background support (works in Flatpak and on host)
const LAUNCHER_OPTIONS_PATCHED =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}else if("linux"===process.platform)e.path=process.env.APPIMAGE||i.getPath("exe"),e.isHidden=!0;d=__zaloWrapLinuxLauncher(new r(e))';

const COMPACT_LAUNCHER_OPTIONS_ORIGINAL =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}u=new r(e)';
const COMPACT_LAUNCHER_OPTIONS_PATCHED =
  'else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}else if("linux"===process.platform)e.path=process.env.APPIMAGE||i.getPath("exe"),e.isHidden=!0;u=__zaloWrapLinuxLauncher(new r(e))';

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
  'checkAutoLaunchEnable:async e=>{try{const{getZaloLauncher:t}=n("$1"),o=t();' +
  'return!!(o&&"function"==typeof o.isEnabled)&&!!await o.isEnabled()}catch(e){return!1}},' +
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
  if (content.includes('__zaloWrapLinuxLauncher') && content.includes('linux_auto_launch_error')) {
    return content;
  }

  content = injectHelper(content);

  // Attempt AST detection for getZaloLauncher variable
  try {
    const root = parseAst(content);
    const getter = root.find('getZaloLauncher: () => $VAR');
    if (getter) {
      const varName = getter.getMatch('VAR').text();
      const win32Assign = root.find(`else if ("win32" === process.platform) { $$$BODY } ${varName} = new $R($E)`);
      if (win32Assign) {
        const r = win32Assign.getMatch('R').text();
        const e = win32Assign.getMatch('E').text();
        const edit = win32Assign.replace(
          `${win32Assign.text().replace(new RegExp(`${varName}\\s*=\\s*new\\s*${r}\\(${e}\\)$`), '')}else if("linux"===process.platform)e.path=process.env.APPIMAGE||i.getPath("exe"),e.isHidden=!0;${varName}=__zaloWrapLinuxLauncher(new ${r}(${e}))`
        );
        content = applyEdits(root, [edit]);
        logger.dim(`Patched launcher initialization via AST for variable: ${varName}`);
      }
    }
  } catch (err) {
    logger.warn('AST launcher detection skipped, falling back:', err.message);
  }

  const isMainVariant = content.includes(LAUNCHER_OPTIONS_ORIGINAL) ||
    content.includes(LAUNCHER_OPTIONS_PATCHED) || content.includes(GET_LAUNCHER_PATCHED);
  const isCompactVariant = content.includes(COMPACT_LAUNCHER_OPTIONS_ORIGINAL) ||
    content.includes(COMPACT_LAUNCHER_OPTIONS_PATCHED) || content.includes(COMPACT_GET_LAUNCHER_PATCHED);
  content = injectHelper(content);

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
