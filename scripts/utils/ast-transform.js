'use strict';

const { parse, Lang } = require('@ast-grep/napi');

const REGEX_PREFIX_KEYWORDS = new Set([
 'return', 'throw', 'case', 'delete', 'void', 'typeof',
 'in', 'instanceof', 'new', 'else', 'do', 'yield', 'await',
]);

/**
 * Parses JavaScript source into an ast-grep root node.
 * @param {string} source - JavaScript source text.
 * @returns {import('@ast-grep/napi').SgNode}
 */
function parseAst(source) {
 const sg = parse(Lang.JavaScript, source);
 return sg.root();
}

/**
 * Safely applies an array of ast-grep edits to the root.
 * @param {import('@ast-grep/napi').SgNode} root
 * @param {Array<import('@ast-grep/napi').Edit>} edits
 * @returns {string} Transformed source text.
 */
function applyEdits(root, edits) {
 if (!edits || edits.length === 0) {
  return root.text();
 }
 return root.commitEdits(edits);
}

function isIdentChar(ch) {
 return (ch >= 65 && ch <= 90) || (ch >= 97 && ch <= 122) || (ch >= 48 && ch <= 57) || ch === 95 || ch === 36;
}

function skipQuoted(source, start, quote) {
 const len = source.length;
 let i = start + 1;
 while (i < len) {
  const c = source.charCodeAt(i);
  if (c === 92) { i += 2; continue; }
  if (c === quote) return i + 1;
  i++;
 }
 return len;
}

function skipRegex(source, start) {
 const len = source.length;
 let i = start + 1;
 let inClass = false;
 while (i < len) {
  const c = source.charCodeAt(i);
  if (c === 92) { i += 2; continue; }
  if (c === 91) inClass = true;
  else if (c === 93) inClass = false;
  else if (c === 47 && !inClass) return i + 1;
  else if (c === 10 || c === 13) return i;
  i++;
 }
 return len;
}

function skipTemplate(source, start) {
 const len = source.length;
 let i = start + 1;
 while (i < len) {
  const c = source.charCodeAt(i);
  if (c === 92) { i += 2; continue; }
  if (c === 96) return i + 1;
  if (c === 36 && source.charCodeAt(i + 1) === 123) {
   i += 2;
   let depth = 1;
   let canRegex = true;
   while (i < len && depth > 0) {
    const tc = source.charCodeAt(i);
    if (tc === 32 || tc === 9 || tc === 10 || tc === 13) { i++; continue; }
    if (tc === 39 || tc === 34) { i = skipQuoted(source, i, tc); canRegex = false; continue; }
    if (tc === 96) { i = skipTemplate(source, i); canRegex = false; continue; }
    if (tc === 47) {
     const next = source.charCodeAt(i + 1);
     if (next === 47) { i += 2; while (i < len && source.charCodeAt(i) !== 10 && source.charCodeAt(i) !== 13) i++; continue; }
     if (next === 42) { i += 2; while (i < len && !(source.charCodeAt(i) === 42 && source.charCodeAt(i + 1) === 47)) i++; i += 2; continue; }
     if (canRegex) { i = skipRegex(source, i); canRegex = false; continue; }
     canRegex = true; i++; continue;
    }
    if (tc === 123) { depth++; canRegex = true; i++; continue; }
    if (tc === 125) { depth--; canRegex = false; i++; continue; }
    canRegex = !isIdentChar(tc) && tc !== 41 && tc !== 93;
    i++;
   }
   continue;
  }
  i++;
 }
 return len;
}

/**
 * Shared JS lexical brace scanner. Invokes `onBraceOpen(pos)` / `onBraceClose(pos)`
 * for real code braces only, skipping strings, templates, comments, and regex literals.
 */
function scanJsBraces(source, startIndex, { initialCanRegex = true, onStep, onBraceOpen, onBraceClose }) {
 const len = source.length;
 let i = startIndex;
 let canRegex = initialCanRegex;

 while (i < len) {
  if (onStep && onStep(i) === true) return;
  const ch = source.charCodeAt(i);

  if (ch === 32 || ch === 9 || ch === 10 || ch === 13) { i++; continue; }
  if (ch === 39 || ch === 34) { i = skipQuoted(source, i, ch); canRegex = false; continue; }
  if (ch === 96) { i = skipTemplate(source, i); canRegex = false; continue; }

  if (ch === 47) {
   const next = source.charCodeAt(i + 1);
   if (next === 47) { i += 2; while (i < len && source.charCodeAt(i) !== 10 && source.charCodeAt(i) !== 13) i++; continue; }
   if (next === 42) { i += 2; while (i < len && !(source.charCodeAt(i) === 42 && source.charCodeAt(i + 1) === 47)) i++; i += 2; continue; }
   if (canRegex) { i = skipRegex(source, i); canRegex = false; continue; }
   canRegex = true;
   i++;
   continue;
  }

  if (ch === 123) {
   if (onBraceOpen && onBraceOpen(i) === true) return;
   canRegex = true;
   i++;
   continue;
  }

  if (ch === 125) {
   if (onBraceClose && onBraceClose(i) === true) return;
   canRegex = false;
   i++;
   continue;
  }

  if (isIdentChar(ch)) {
   const identStart = i;
   i++;
   while (i < len && isIdentChar(source.charCodeAt(i))) i++;
   const word = source.slice(identStart, i);
   canRegex = REGEX_PREFIX_KEYWORDS.has(word);
   continue;
  }

  // Postfix ++ / -- leaves an expression value, so a following '/' is division, not regex.
  if ((ch === 43 || ch === 45) && i > 0 && source.charCodeAt(i - 1) === ch) {
   canRegex = false;
   i++;
   continue;
  }

  canRegex = ch !== 41 && ch !== 93;
  i++;
 }
}

function finalizeBlock(source, startBrace, endBracePlusOne, includePrefixRe) {
 let start = startBrace;
 if (includePrefixRe) {
  const lookback = source.slice(Math.max(0, start - 160), start);
  const m = lookback.match(includePrefixRe);
  if (m) start -= m[0].length;
 }
 return { start, end: endBracePlusOne, snippet: source.slice(start, endBracePlusOne) };
}

/**
 * Extracts the balanced `{ ... }` block enclosing `anchorIndex`.
 */
function extractEnclosingBlock(source, anchorIndex, { levels = 1, includePrefixRe = null } = {}) {
 if (anchorIndex < 0 || anchorIndex >= source.length) return null;

 const braceStack = [];
 let targetStart = -1;
 let targetDepth = -1;
 let result = null;

 scanJsBraces(source, 0, {
  initialCanRegex: true,
  onStep: (i) => {
   if (targetStart === -1 && i >= anchorIndex) {
    if (braceStack.length < levels) return true;
    targetDepth = braceStack.length - levels;
    targetStart = braceStack[targetDepth];
   }
   return false;
  },
  onBraceOpen: (i) => {
   braceStack.push(i);
   return false;
  },
  onBraceClose: (i) => {
   braceStack.pop();
   if (targetStart !== -1 && braceStack.length === targetDepth) {
    result = finalizeBlock(source, targetStart, i + 1, includePrefixRe);
    return true;
   }
   return false;
  },
 });

 return result;
}

/**
 * Extracts a balanced `{ ... }` block starting at a known opening `{` index.
 */
function extractBlockFrom(source, openBraceIndex, { includePrefixRe = null } = {}) {
 if (openBraceIndex < 0 || openBraceIndex >= source.length || source.charCodeAt(openBraceIndex) !== 123) {
  return null;
 }

 let depth = 0;
 let result = null;

 scanJsBraces(source, openBraceIndex, {
  initialCanRegex: false,
  onBraceOpen: () => {
   depth++;
   return false;
  },
  onBraceClose: (i) => {
   depth--;
   if (depth === 0) {
    result = finalizeBlock(source, openBraceIndex, i + 1, includePrefixRe);
    return true;
   }
   return false;
  },
 });

 return result;
}

module.exports = {
 parseAst,
 applyEdits,
 extractEnclosingBlock,
 extractBlockFrom,
 Lang,
};
