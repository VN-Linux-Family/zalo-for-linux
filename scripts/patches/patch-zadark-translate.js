'use strict';

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { APP_DIR, ZADARK_VENDOR_DIR } = require('../utils/paths');

const OLD_TRANSLATE_MIN =
  '(async(e,a)=>{try{const n=await fetch("https://api.zadark.com/v1/translate",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({text:e,target:a})});return await n.json()}catch(e){return{success:!1,message:e.message}}})(d,c)';

const NEW_TRANSLATE_MIN =
  '(async(e,a)=>{try{const t=e.replace(/<br\\s*\\/?>/gi,"\\n"),r=await fetch("https://clients5.google.com/translate_a/t?client=dict-chrome-ex&sl=auto&tl="+encodeURIComponent(a)+"&q="+encodeURIComponent(t));if(!r.ok)throw new Error("HTTP "+r.status);const s=await r.json(),l=Array.isArray(s[0])?s[0][0]:s[0],i=(Array.isArray(s[0])&&s[0][1]||"auto").toLowerCase(),m={vi:"Việt",en:"Anh",zh:"Trung","zh-cn":"Trung (Giản thể)","zh-tw":"Trung (Phồn thể)",ja:"Nhật",ko:"Hàn",fr:"Pháp",de:"Đức",ru:"Nga",es:"Tây Ban Nha",th:"Thái",lo:"Lào",km:"Khmer",id:"Indonesia",ms:"Mã Lai",it:"Ý",pt:"Bồ Đào Nha",ar:"Ả Rập",hi:"Hindi"},o=m[i]||i.toUpperCase();return{success:!0,translation:String(l).replace(/\\n/g,"<br>"),languageName:"Tiếng "+o}}catch(e){return{success:!1,message:e.message.indexOf("<")!==-1?"Không thể kết nối máy chủ dịch":e.message}}})(d,c)';

function patchFile(filePath) {
  if (!fs.existsSync(filePath)) return false;
  let content = fs.readFileSync(filePath, 'utf8');
  if (content.includes('clients5.google.com')) {
    return false; // Already patched
  }
  if (!content.includes(OLD_TRANSLATE_MIN)) {
    return false;
  }
  content = content.replace(OLD_TRANSLATE_MIN, NEW_TRANSLATE_MIN);
  fs.writeFileSync(filePath, content, 'utf8');
  return true;
}

async function main(appDir = APP_DIR) {
  let patchedCount = 0;

  // 1. Patch prebuilt ZaDark vendor asset
  const vendorAsset = path.join(
    ZADARK_VENDOR_DIR,
    'build',
    'pc',
    'assets',
    'js',
    'zadark-translate.min.js'
  );
  if (patchFile(vendorAsset)) {
    patchedCount++;
    logger.dim('Patched vendor ZaDark translate asset to use Google Translate');
  }

  // 2. Patch extracted/integrated app if present
  const appAsset = path.join(
    appDir,
    'pc-dist',
    'zadark',
    'js',
    'zadark-translate.min.js'
  );
  if (patchFile(appAsset)) {
    patchedCount++;
    logger.dim('Patched app ZaDark translate asset to use Google Translate');
  }

  if (patchedCount > 0) {
    logger.success(
      `ZaDark translate patched to Google Translate (${patchedCount} file(s))`
    );
  }
}

if (require.main === module) {
  main().catch((err) => {
    logger.error('patch-zadark-translate failed:', err.message);
    process.exit(1);
  });
}

module.exports = {
  main,
  patchFile,
  OLD_TRANSLATE_MIN,
  NEW_TRANSLATE_MIN,
};
