'use strict';

const { parse, Lang } = require('@ast-grep/napi');

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

module.exports = {
 parseAst,
 applyEdits,
 Lang,
};
