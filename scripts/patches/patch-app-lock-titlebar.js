'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');
const { APP_PC_DIST } = require('../utils/paths');

const MARKER = 'fa-Lock_24_Line';
const HOVER_MARKER = '/* app-lock-titlebar-hover */';
const LOCK_TITLE_MARKER = '/* app-lock-title-left */';
const HOVER_RULE = `${HOVER_MARKER}.btn.titlebar__menu__btn.fa-Lock_24_Line:hover{background:var(--button-secondary-neutral-hover)}.btn.titlebar__menu__btn.fa-Lock_24_Line:active{background:var(--button-secondary-neutral-hover);transition:none}`;
const LOCK_TITLE_RULE = `${LOCK_TITLE_MARKER}#titleBar.locked .title-name{padding-left:0!important}`;
const INSERTION = ',null,this.state.pendingUpdate?';
function buildLockButton(react) {
  return `this.props.isPopupWindow||!this.props.status||this.props.status.isAppLock?null:${react}.createElement("i",{className:"btn titlebar__menu__btn fa fa-Lock_24_Line",style:{position:"fixed",left:"calc(env(titlebar-area-width) - 46px)",top:"0",width:"46px",minWidth:"46px",flex:"0 0 46px",height:"32px",maxHeight:"32px",display:"flex",alignItems:"center",justifyContent:"center",zIndex:3},onClick:this._showAppLock.bind(this)})`;
}

function enclosingClass(node) {
  for (let parent = node.parent(); parent; parent = parent.parent()) {
    if (['class_declaration', 'class_expression'].includes(parent.kind())) return parent;
  }
  return null;
}

function patchContent(content) {
  if (content.includes(MARKER)) {
    if (!content.includes('_showAppLock')) throw new Error('patch-app-lock-titlebar: lock marker exists without app-lock handler');
    if (content.includes('className:"btn titlebar__menu__btn fa fa-Lock_24_Line"') && content.includes('left:"calc(env(titlebar-area-width) - 46px)"') && content.includes('maxHeight:"32px"')) {
      parseAst(content);
      return { content, status: 'already' };
    }
    const button = /([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)\.createElement\("i",\{className:"btn titlebar__menu__btn (?:fa )?fa-Lock_24_Line"(?:,style:\{[^}]*\})?,onClick:this\._showAppLock\.bind\(this\)\}\)/;
    if (button.test(content)) {
      const upgraded = content.replace(button, (_, react) => `${react}.createElement("i",{className:"btn titlebar__menu__btn fa fa-Lock_24_Line",style:{position:"fixed",left:"calc(env(titlebar-area-width) - 46px)",top:"0",width:"46px",minWidth:"46px",flex:"0 0 46px",height:"32px",maxHeight:"32px",display:"flex",alignItems:"center",justifyContent:"center",zIndex:3},onClick:this._showAppLock.bind(this)})`);
      parseAst(upgraded);
      return { content: upgraded, status: 'upgraded' };
    }
    parseAst(content);
    return { content, status: 'already' };
  }

  const root = parseAst(content);
  const renders = root.findAll({ rule: { kind: 'method_definition' } }).filter(method => {
    const name = method.field('name')?.text().replace(/["']/g, '');
    if (name !== 'render' || !method.text().includes('titleBar')) return false;
    const owner = enclosingClass(method);
    return !!owner && owner.text().includes('_showAppLock');
  });
  if (renders.length !== 1) throw new Error(`patch-app-lock-titlebar: expected one app-lock titlebar render method, found ${renders.length}`);

  const render = renders[0];
  const source = render.text();
  const matches = source.split(INSERTION).length - 1;
  if (matches !== 1) throw new Error(`patch-app-lock-titlebar: expected one titlebar lock insertion point, found ${matches}`);
  const createElement = render.findAll({ rule: { kind: 'call_expression' } }).find(call =>
    call.field('function')?.text().endsWith('.createElement')
  );
  const react = createElement?.field('function')?.text().replace(/\.createElement$/, '');
  if (!react) throw new Error('patch-app-lock-titlebar: cannot identify React createElement alias');
  const updated = source.replace(INSERTION, `,${buildLockButton(react)},this.state.pendingUpdate?`);
  const output = applyEdits(root, [render.replace(updated)]);
  parseAst(output);
  if (!output.includes(MARKER) || !output.includes('_showAppLock.bind(this)')) {
    throw new Error('patch-app-lock-titlebar: injected lock button did not verify');
  }
  return { content: output, status: 'patched' };
}

function patchStylesheet(content) {
  const missing = [!content.includes(HOVER_MARKER) && HOVER_RULE, !content.includes(LOCK_TITLE_MARKER) && LOCK_TITLE_RULE].filter(Boolean);
  if (!missing.length) return { content, status: 'already' };
  return { content: `${content}\n${missing.join('\n')}\n`, status: 'patched' };
}

function findTargets(directory) {
  if (!fs.existsSync(directory)) throw new Error(`patch-app-lock-titlebar: pc-dist directory not found: ${directory}`);
  return fs.readdirSync(directory, { withFileTypes: true })
    .filter(entry => entry.isFile() && /^compact-app-pc\..+\.js$/.test(entry.name))
    .map(entry => path.join(directory, entry.name)).sort();
}

async function main() {
  const targets = findTargets(APP_PC_DIST);
  if (targets.length !== 1) throw new Error(`patch-app-lock-titlebar: expected one compact-app-pc bundle, found ${targets.length}`);
  const file = targets[0];
  const result = patchContent(fs.readFileSync(file, 'utf8'));
  if (result.status !== 'already') fs.writeFileSync(file, result.content, 'utf8');
  logger.dim(`App lock titlebar ${result.status}: ${path.basename(file)}`);
  const cssFiles = fs.readdirSync(APP_PC_DIST).filter(name => /^(?:compact-app-pc|default-login-startup.*|login-startup)\..+\.css$/.test(name));
  for (const cssName of cssFiles) {
    const cssFile = path.join(APP_PC_DIST, cssName);
    const css = patchStylesheet(fs.readFileSync(cssFile, 'utf8'));
    if (css.status !== 'already') fs.writeFileSync(cssFile, css.content, 'utf8');
    logger.dim(`App lock titlebar styles ${css.status}: ${cssName}`);
  }
  if (!cssFiles.some(name => name.startsWith('default-login-startup')) || !cssFiles.some(name => name.startsWith('compact-app-pc.'))) {
    throw new Error('patch-app-lock-titlebar: required compact-app-pc and default-login-startup stylesheets not found');
  }
}

if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
module.exports = { main, patchContent, patchStylesheet, findTargets, MARKER, HOVER_MARKER, LOCK_TITLE_MARKER, buildLockButton };
