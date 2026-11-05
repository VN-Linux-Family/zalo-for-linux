#!/usr/bin/env node
"use strict";
const { execSync, execFileSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { createHash } = require('node:crypto');
const { parseAst } = require('../utils/ast-transform');

const APP_DIR = path.join(__dirname, '..', '..', 'app');
const ZOCR_DIR = path.join(__dirname, '..', '..', 'zocr');
const TEMP_DIR = path.join(__dirname, '..', '..', 'temp');
const ONNXRUNTIME_URL = `https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-${process.arch === 'arm64' ? 'aarch64' : 'x64'}-1.30.0.tgz`;

// Extract the 32-byte seed used by the OCR model vault.
function extractKey(data) {
  function requireBytes(offset, length) {
    if (offset < 0 || offset + length > data.length) {
      throw new Error('Truncated host executable.');
    }
  }

  requireBytes(0, 64);
  if (data.toString('ascii', 0, 2) !== 'MZ') {
    throw new Error('Missing DOS MZ signature.');
  }
  const peOffset = data.readUInt32LE(0x3c);
  requireBytes(peOffset, 24);
  if (!data.subarray(peOffset, peOffset + 4).equals(Buffer.from('PE\0\0'))) {
    throw new Error('Missing PE signature.');
  }
  const count = data.readUInt16LE(peOffset + 6);
  const table = peOffset + 24 + data.readUInt16LE(peOffset + 20);
  requireBytes(table, count * 40);
  requireBytes(peOffset + 24, 2);
  const magic = data.readUInt16LE(peOffset + 24);
  const imageBase = magic === 0x20b ? Number(data.readBigUInt64LE(peOffset + 48)) : data.readUInt32LE(peOffset + 52);

  const sections = [];
  for (let i = 0; i < count; i++) {
    const entry = table + i * 40;
    sections.push({
      name: data.toString('latin1', entry, entry + 8).replace(/\0+$/, ''),
      virtualSize: data.readUInt32LE(entry + 8),
      rva: data.readUInt32LE(entry + 12),
      rawSize: data.readUInt32LE(entry + 16),
      rawOffset: data.readUInt32LE(entry + 20),
    });
  }
  const text = sections.findLast(section => section.name === '.text');
  if (!text) {
    throw new Error('Missing executable .text section.');
  }
  requireBytes(text.rawOffset, text.rawSize);
  const textDigest = createHash('sha256')
    .update(data.subarray(text.rawOffset, text.rawOffset + text.rawSize))
    .digest('hex');

  function chunk(rva) {
    const section = sections.find(section =>
      rva >= section.rva && rva < section.rva + Math.max(section.virtualSize, section.rawSize));
    if (!section) throw new Error(`Unmapped RVA: 0x${rva.toString(16)}`);
    const offset = section.rawOffset + rva - section.rva;
    requireBytes(offset, 32);
    return data.subarray(offset, offset + 32);
  }
  // Dynamic signature scanner discovered via REA (Reverse Engineer Anything):
  // Disassembly of zocr-host.exe at function .text:0x1403b501f shows that the binary
  // derives the 32-byte model vault seed key using four 16-byte RIP-relative loads
  // (movups 0F 10 05) followed by SIMD XORs (xorps 0F 57).
  function scanKeyBySignature() {
    const textBytes = data.subarray(text.rawOffset, text.rawOffset + text.rawSize);
    for (let i = 0; i < textBytes.length - 100; i++) {
      if (textBytes[i] === 0x0f && textBytes[i + 1] === 0x57) {
        for (let j = i + 2; j < i + 30; j++) {
          if (textBytes[j] === 0x0f && textBytes[j + 1] === 0x57) {
            const movups = [];
            for (let k = Math.max(0, i - 120); k < i; k++) {
              if (textBytes[k] === 0x0f && textBytes[k + 1] === 0x10 && textBytes[k + 2] === 0x05) {
                const disp = textBytes.readInt32LE(k + 3);
                const rip = imageBase + text.rva + k + 7;
                const targetRva = rip + disp - imageBase;
                movups.push(targetRva);
              }
            }
            if (movups.length >= 4) {
              const sorted = [...new Set(movups)].sort((a, b) => a - b);
              for (let u = 0; u < sorted.length; u++) {
                for (let v = u + 1; v < sorted.length; v++) {
                  const r1 = sorted[u];
                  const r2 = sorted[v];
                  if (r2 - r1 === 32) {
                    try {
                      const c1 = chunk(r1);
                      const c2 = chunk(r2);
                      const derived = Buffer.from(c1.map((b, idx) => b ^ c2[idx])).toString('hex');
                      if (!/^0+$|^f+$/i.test(derived)) {
                        logger.dim(`Dynamically extracted OCR seed key via signature scan (RVAs: 0x${r1.toString(16)}, 0x${r2.toString(16)})`);
                        return {
                          seed_key: derived,
                          text_digest_key: textDigest,
                        };
                      }
                    } catch {
                      // Ignore unmapped RVA errors and continue scanning
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
    return null;
  }

  const scanned = scanKeyBySignature();
  if (scanned) {
    return scanned;
  }

  logger.warn('Signature scanner found no candidate; falling back to legacy RVAs');
  const first = chunk(0x4411f6);
  const second = chunk(0x441216);
  return {
    seed_key: Buffer.from(first.map((byte, i) => byte ^ second[i])).toString('hex'),
    text_digest_key: textDigest,
  };
}


function patchWithAst(source) {
  const root = parseAst(source);
  const edits = [];

  // 1. Array containing "zocr-host.exe"
  const hostLiteral = root.findAll('"zocr-host.exe"').find(l => l.parent().kind() === 'array');
  if (hostLiteral) {
    edits.push(hostLiteral.parent().replace(
      'process.platform==="linux"?["zocr-host","libonnxruntime.so","zocr/models.zmdl","seed_key","text_digest_key"]:["zocr-host.exe"]'
    ));
  }

  // 2. Executable name: replace (0, $.join)(e, "zocr-host.exe")
  const joinCall = root.find('(0, $JOIN)($E, "zocr-host.exe")');
  if (joinCall) {
    const joinFn = joinCall.getMatch('JOIN').text();
    const dirArg = joinCall.getMatch('E').text();
    edits.push(joinCall.replace(
      `(0, ${joinFn})(${dirArg}, process.platform==="linux"?"zocr-host":"zocr-host.exe")`
    ));
  }

  // 3. hostRuntimeDir: function returning (..., "plugins", "ocr")
  const pluginsOcrCall = root.find('(0, $JOIN)($ARG, "plugins", "ocr")');
  if (pluginsOcrCall) {
    let fn = pluginsOcrCall;
    while (fn && fn.kind() !== 'function_declaration' && fn.kind() !== 'function') {
      fn = fn.parent();
    }
    if (fn) {
      const fnBody = fn.field('body');
      edits.push(fnBody.replace(
        `{if(process.platform==="linux")return(0,$.join)(__dirname,"../../ocr");${fnBody.text().slice(1)}`
      ));
    }
  }

  // 4. Platform check in switch: case "win32" in the platform switch
  const win32SwitchCase = root.findAll('"win32"').map(l => l.parent()).find(p => p.kind() === 'switch_case');
  if (win32SwitchCase) {
    edits.push(win32SwitchCase.replace(
      `case "linux":if(G!=="x64"&&G!=="arm64")k=new Error(\`Unsupported architecture on Linux: \${G}\`);break;${win32SwitchCase.text()}`
    ));
  }

  // 5. isSupported / unsupportedReason function
  const os32bitLiteral = root.find('"os-32bit"');
  if (os32bitLiteral) {
    let fn = os32bitLiteral;
    while (fn && fn.kind() !== 'function_declaration' && fn.kind() !== 'function') {
      fn = fn.parent();
    }
    if (fn) {
      const fnBody = fn.field('body');
      edits.push(fnBody.replace(
        `{if(process.platform==="linux")return G==="x64"||G==="arm64"?null:"unsupported-platform";${fnBody.text().slice(1)}`
      ));
    }
  }

  // 6. Engine creation if (K === "win32")
  const engineIf = root.find('if ($K === "win32") { let { engine: $T, pipeTransport: $I } = $GR($E); $$$BODY }');
  if (engineIf) {
    const cond = engineIf.field('condition');
    edits.push(cond.replace(`(${cond.text()} || process.platform === "linux")`));
  }

  if (edits.length === 0) {
    return null;
  }

  logger.dim(`Applied ${edits.length} AST transformation(s) to zocr.min.js`);
  return root.commitEdits(edits);
}

function patch(source) {
  if (source.includes('process.platform==="linux"') || source.includes('process.platform === "linux"')) {
    return source;
  }

  try {
    const astPatched = patchWithAst(source);
    if (astPatched) return astPatched;
  } catch (err) {
    logger.warn('AST patcher encountered an issue, trying legacy replacement fallback:', err.message);
  }

  // Fallback to string replacement
  const replacements = [
    ['Ce=["zocr-host.exe"', 'Ce=process.platform==="linux"?["zocr-host","libonnxruntime.so","zocr/models.zmdl","seed_key","text_digest_key"]:["zocr-host.exe"'],
    ['join)(e,"zocr-host.exe")', 'join)(e,process.platform==="linux"?"zocr-host":"zocr-host.exe")'],
    ['function We(e,r){return e?', 'function We(e,r){if(process.platform==="linux")return(0,$.join)(__dirname,"../../ocr");return e?'],
    ['switch(K){case"win32":', 'switch(K){case"linux":if(G!=="x64"&&G!=="arm64")k=new Error(`Unsupported architecture on Linux: ${G}`);break;case"win32":'],
    ['function Ie(){return K===', 'function Ie(){if(K==="linux")return G==="x64"||G==="arm64"?null:"unsupported-platform";return K==='],
    ['if(K==="win32"){let{engine:', 'if(K==="win32"||K==="linux"){let{engine:'],
  ];
  for (const [original, replacement] of replacements) {
    if (source.split(replacement).length === 2) continue;
    if (source.split(original).length !== 2) {
      throw new Error(`loader does not match expected source: ${original}`);
    }
    source = source.replace(original, replacement);
  }
  return source;
}

function build() {
  logger.info('Building zocr-host from source...');
  if (!fs.existsSync(path.join(ZOCR_DIR, 'Cargo.toml'))) {
    logger.warn('zocr not found, skipping');
    return false;
  }

  const rootDir = path.join(__dirname, '..', '..');
  try {
    execSync('cargo build --release -p zocr-host', { cwd: rootDir, stdio: 'pipe' });
  } catch (error) {
    logger.error('Failed to build zocr-host', error.message);
    if (error.stdout) logger.dim(error.stdout.toString());
    if (error.stderr) logger.dim(error.stderr.toString());
    throw new Error('Failed to build zocr-host');
  }

  const rootBinary = path.join(rootDir, 'target', 'release', 'zocr-host');
  const localBinary = path.join(ZOCR_DIR, 'target', 'release', 'zocr-host');
  const sourceBinary = fs.existsSync(rootBinary) ? rootBinary : localBinary;

  if (fs.existsSync(rootBinary)) {
    fs.ensureDirSync(path.join(ZOCR_DIR, 'target', 'release'));
    fs.copyFileSync(rootBinary, localBinary);
  }

  const destDir = path.join(APP_DIR, 'native', 'ocr');
  fs.ensureDirSync(destDir);
  fs.copyFileSync(
    sourceBinary,
    path.join(destDir, 'zocr-host')
  );
  const version = process.env.ZALO_WIN_VERSION;
  if (!version) throw new Error('ZALO_WIN_VERSION is required; run prepare-app.js first');
  const pluginsDir = path.join(TEMP_DIR, `Zalo-Win-${version}`, `Zalo-${version}`, 'plugins', 'ocr');
  for (const file of ['zocr/models.zmdl', 'THIRD_PARTY_NOTICES.txt']) {
    fs.copySync(path.join(pluginsDir, file), path.join(destDir, file));
    logger.dim(`Copied ${file} to ${path.relative(APP_DIR, destDir)}`);
  }
  const keys = extractKey(fs.readFileSync(path.join(pluginsDir, 'zocr-host.exe')));
  for (const [name, value] of Object.entries(keys)) {
    fs.writeFileSync(path.join(destDir, name), value + '\n', { mode: 0o600 });
    logger.dim(`Key written to ${path.relative(APP_DIR, destDir)}/${name}`);
  }
  logger.success('zocr-host built and installed');
  return true;
}

function installOnnxRuntime() {
  if (process.arch !== 'x64' && process.arch !== 'arm64') {
    throw new Error(`Unsupported architecture for ONNX Runtime: ${process.arch}`);
  }
  fs.ensureDirSync(TEMP_DIR);
  const filename = path.basename(new URL(ONNXRUNTIME_URL).pathname);
  const archive = path.join(TEMP_DIR, filename);
  if (!fs.existsSync(archive)) {
    logger.info('Downloading ONNX Runtime...');
    try {
      execFileSync('wget', ['--progress=bar:force', ONNXRUNTIME_URL, '-O', archive], { stdio: 'inherit' });
    } catch (error) {
      fs.removeSync(archive);
      throw new Error(`Failed to download ONNX Runtime: ${error.message}`);
    }
  } else {
    logger.dim('Using cached ONNX Runtime archive');
  }

  logger.info('Extracting ONNX Runtime...');
  const extractDir = path.join(TEMP_DIR, filename.replace(/\.tgz$/, ''));
  fs.ensureDirSync(extractDir);
  execFileSync('tar', ['-xzf', archive, '-C', extractDir, '--strip-components=1'], { stdio: 'pipe' });
  const destDir = path.join(APP_DIR, 'native', 'ocr');
  fs.ensureDirSync(destDir);
  fs.copySync(
    fs.realpathSync(path.join(extractDir, 'lib', 'libonnxruntime.so')),
    path.join(destDir, 'libonnxruntime.so')
  );
  logger.success('libonnxruntime.so installed');
}

function main() {
  if (!build()) return;
  installOnnxRuntime();

  logger.info('Patching OCR runtime for Linux...');
  const file = path.join(APP_DIR, 'native', 'nativelibs', 'zocr', 'zocr.min.js');
  const source = fs.readFileSync(file, 'utf8');
  const updated = patch(source);
  if (updated === source) {
    logger.success('OCR runtime already patched');
    return;
  }
  fs.writeFileSync(file, updated, 'utf8');
  logger.dim(`Patched ${path.relative(APP_DIR, file)}`);
  logger.success('OCR runtime patched for Linux');
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    logger.error('Failed to patch OCR runtime', error.message);
    process.exitCode = 1;
  }
}

module.exports = { main, extractKey, patch };
