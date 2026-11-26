'use strict';

const dictionary = require('./locale.json').messages;
function buildTrayMenuTemplate({ locale, visible, updateItems = [], devModeItems = [], onToggleVisibility, onQuit } = {}) {
  const language = locale === 'en' ? 'en' : 'vi';
  const t = (key) => dictionary['tray-menu.' + key][language];
  return [
    { label: visible ? t('hide') : t('show'), click: onToggleVisibility },
    ...updateItems,
    ...devModeItems,
    { label: t('quit'), click: onQuit }
  ];
}
module.exports = { buildTrayMenuTemplate };
