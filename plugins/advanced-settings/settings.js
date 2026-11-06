(() => {
  'use strict';

  const advanced = document.getElementById('advanced-view');
  const manager = document.getElementById('userscripts-view');
  const openButton = document.getElementById('open-userscripts');
  const backButton = document.getElementById('back-to-advanced');
  const forms = [...document.querySelectorAll('[data-sound]')];
  let preview = null;
  let previewButton = null;
  let previewRequest = 0;

  function message(form, text, error = false) {
    const status = form.querySelector('.sound-status');
    status.textContent = text;
    status.classList.toggle('error', error);
  }

  function errorMessage(error) {
    return String(error.message || error).replace(/^Error invoking remote method '[^']+': Error:\s*/, '');
  }

  function stopPreview() {
    previewRequest++;
    if (preview) { preview.pause(); preview = null; }
    if (previewButton) {
      previewButton.textContent = 'Nghe thử';
      previewButton.setAttribute('aria-pressed', 'false');
      previewButton = null;
    }
  }

  function showManager(show) {
    stopPreview();
    document.body.classList.toggle('show-userscripts', show);
    advanced.inert = show;
    manager.inert = !show;
    advanced.setAttribute('aria-hidden', String(show));
    manager.setAttribute('aria-hidden', String(!show));
    (show ? backButton : openButton).focus();
    document.title = show ? 'Userscript Manager — Nâng cao' : 'Nâng cao';
  }
  openButton.addEventListener('click', () => showManager(true));
  backButton.addEventListener('click', () => showManager(false));

  for (const form of forms) {
    const kind = form.dataset.sound;
    const input = form.elements.path;
    const browse = form.querySelector('[data-action="browse"]');
    const reset = form.querySelector('[data-action="reset"]');
    const listen = form.querySelector('[data-action="preview"]');
    listen.setAttribute('aria-pressed', 'false');

    async function save(value) {
      stopPreview();
      const saved = await window.linuxSettings.saveSound(kind, value);
      input.value = saved[kind];
      message(form, value ? 'Đã lưu. Áp dụng từ lần phát tiếp theo.' : 'Đã khôi phục âm thanh mặc định.');
    }

    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      try { await save(input.value); }
      catch (error) { message(form, errorMessage(error), true); }
    });
    input.addEventListener('input', () => { stopPreview(); message(form, 'Chưa lưu thay đổi.'); });
    browse.addEventListener('click', async () => {
      browse.disabled = true;
      try {
        const selected = await window.linuxSettings.pickSound(kind);
        if (selected) {
          stopPreview();
          input.value = selected;
          message(form, 'Bấm Nghe thử để kiểm tra, sau đó Lưu để áp dụng.');
        }
      } catch (error) { message(form, errorMessage(error), true); }
      finally { browse.disabled = false; }
    });
    reset.addEventListener('click', async () => {
      try { await save(''); }
      catch (error) { message(form, errorMessage(error), true); }
    });
    listen.addEventListener('click', async () => {
      const wasPlaying = previewButton === listen;
      stopPreview();
      if (wasPlaying) return;
      const request = previewRequest;
      try {
        const customUrl = await window.linuxSettings.previewSound(kind, input.value);
        if (request !== previewRequest) return;
        const sound = new Audio(customUrl);
        preview = sound;
        previewButton = listen;
        listen.textContent = 'Dừng nghe thử';
        listen.setAttribute('aria-pressed', 'true');
        sound.addEventListener('ended', () => { if (preview === sound) stopPreview(); });
        sound.addEventListener('error', () => {
          if (preview !== sound) return;
          stopPreview();
          message(form, 'Không phát được tệp âm thanh này. Hãy chọn tệp khác.', true);
        });
        await sound.play();
      } catch (error) {
        if (request !== previewRequest) return;
        stopPreview();
        message(form, errorMessage(error), true);
      }
    });
  }

  const keepRecalled = document.getElementById('keep-recalled');
  const recalledStatus = document.getElementById('recalled-status');
  function recalledMessage(text, error = false) {
    recalledStatus.textContent = text;
    recalledStatus.classList.toggle('error', error);
  }
  keepRecalled.disabled = true;
  keepRecalled.addEventListener('change', async () => {
    const enabled = keepRecalled.checked;
    if (!enabled && !window.confirm('Tắt tính năng sẽ xoá các tin nhắn đã thu hồi đang giữ. Tiếp tục?')) {
      keepRecalled.checked = true;
      return;
    }
    keepRecalled.disabled = true;
    try {
      const saved = await window.linuxSettings.setKeepRecalled(enabled);
      keepRecalled.checked = saved.keepRecalled;
      recalledMessage(saved.keepRecalled ? 'Đã bật. Áp dụng ngay cho cửa sổ Zalo đang mở.' : 'Đã tắt và xoá các tin đã giữ.');
    } catch (error) {
      keepRecalled.checked = !enabled;
      recalledMessage(errorMessage(error), true);
    } finally {
      keepRecalled.disabled = false;
    }
  });

  window.addEventListener('pagehide', stopPreview);
  window.linuxSettings.get().then((settings) => {
    for (const form of forms) form.elements.path.value = settings[form.dataset.sound] || '';
    keepRecalled.checked = settings.keepRecalled === true;
    keepRecalled.disabled = false;
  }).catch((error) => {
    for (const form of forms) message(form, errorMessage(error), true);
    recalledMessage(errorMessage(error), true);
  });
})();
