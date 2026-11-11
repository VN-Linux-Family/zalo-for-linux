'use strict';

(function () {
  let screenshotMessages = {};
  function applyScreenshotLocale(update) {
    if (!update || (update.locale !== 'vi' && update.locale !== 'en')) return;
    const locale = update.locale;
    if (update.messages) screenshotMessages = update.messages;
    const t = (key) => {
      const entry = screenshotMessages['screenshot.' + key];
      return entry ? entry[locale] : key;
    };
    document.documentElement.lang = locale;
    const hint = document.getElementById('hint-tooltip');
    if (hint) hint.textContent = t('hint');
    document.querySelectorAll('[data-tool]').forEach((el) => { el.title = t(el.dataset.i18nKey || el.dataset.tool); });
    const controls = { 'btn-undo': 'undo', 'btn-redo': 'redo', 'btn-save': 'save', 'btn-copy': 'copy', 'btn-cancel': 'cancel', 'btn-accept': 'accept' };
    Object.keys(controls).forEach((id) => { const el = document.getElementById(id); if (el) el.title = t(el.dataset.i18nKey || controls[id]); });
    document.querySelectorAll('[data-size]').forEach((el) => { el.title = t(el.dataset.i18nKey || el.dataset.size); });
  }

  if (window.screencap && typeof window.screencap.onLanguageUpdate === 'function') {
    window.screencap.onLanguageUpdate(applyScreenshotLocale);
  }

  const dpr = window.devicePixelRatio || 1;

  // DOM elements
  const baseCanvas = document.getElementById('base-canvas');
  const drawCanvas = document.getElementById('draw-canvas');
  const maskCanvas = document.getElementById('mask-canvas');
  const baseCtx = baseCanvas.getContext('2d');
  const drawCtx = drawCanvas.getContext('2d');
  const maskCtx = maskCanvas.getContext('2d');

  const toolbar = document.getElementById('toolbar');
  const subToolbar = document.getElementById('sub-toolbar');
  const sizeBadge = document.getElementById('size-badge');
  const hintTooltip = document.getElementById('hint-tooltip');
  const loupe = document.getElementById('loupe');
  const loupeCanvas = document.getElementById('loupe-canvas');
  const loupeCtx = loupeCanvas.getContext('2d');
  const loupeInfo = document.getElementById('loupe-info');
  const textEditor = document.getElementById('text-editor');

  const btnUndo = document.getElementById('btn-undo');
  const btnRedo = document.getElementById('btn-redo');
  const btnSave = document.getElementById('btn-save');
  const btnCopy = document.getElementById('btn-copy');
  const btnCancel = document.getElementById('btn-cancel');
  const btnAccept = document.getElementById('btn-accept');

  // State
  let imageLoaded = false;
  let bgImage = null;

  // Selection: { x, y, w, h } in CSS px
  let sel = null;
  let state = 'NONE'; // 'NONE' | 'SELECTING' | 'SELECTED' | 'RESIZING' | 'MOVING' | 'DRAWING'
  let activeHandle = null;
  let startPt = { x: 0, y: 0 };
  let moveOffset = { x: 0, y: 0 };

  // Tools & annotations
  let activeTool = null; // null | 'rect' | 'circle' | 'arrow' | 'line' | 'doodle' | 'mosaic' | 'text'
  let strokeColor = '#e53935';
  let strokeWidth = 4;
  let annotations = [];
  let undoStack = []; // stores serialized annotations array snapshots

  let currentDrawing = null;

  // Initialize canvas sizes
  function resizeCanvases() {
    const w = window.innerWidth;
    const h = window.innerHeight;

    [baseCanvas, drawCanvas, maskCanvas].forEach((c) => {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
      c.style.width = `${w}px`;
      c.style.height = `${h}px`;
    });

    baseCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
    drawCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
    maskCtx.setTransform(dpr, 0, 0, dpr, 0, 0);

    if (bgImage) {
      baseCtx.drawImage(bgImage, 0, 0, w, h);
    }
    renderMask();
    renderAnnotations();
  }

  window.addEventListener('resize', resizeCanvases);

  // Initialize from preload
  if (window.screencap && window.screencap.onInit) {
    window.screencap.onInit((data) => {
      if (data && data.imageSrc) {
        loadImage(data.imageSrc);
      }
    });
  }

  // Fallback: check query param `?img=...`
  const params = new URLSearchParams(window.location.search);
  const imgUrl = params.get('img');
  if (imgUrl) {
    loadImage(imgUrl);
  }

  function loadImage(src) {
    bgImage = new Image();
    bgImage.onload = () => {
      imageLoaded = true;
      resizeCanvases();
      loupe.style.display = 'block';
    };
    bgImage.src = src;
  }

  // ---------------------------------------------------------------------------
  // Handles & Hit Testing
  // ---------------------------------------------------------------------------

  const HANDLE_RADIUS = 5;

  function getHandles() {
    if (!sel || sel.w <= 0 || sel.h <= 0) return {};
    const { x, y, w, h } = sel;
    const mx = x + w / 2;
    const my = y + h / 2;
    return {
      tl: { x: x, y: y, cursor: 'nwse-resize' },
      tc: { x: mx, y: y, cursor: 'ns-resize' },
      tr: { x: x + w, y: y, cursor: 'nesw-resize' },
      rc: { x: x + w, y: my, cursor: 'ew-resize' },
      br: { x: x + w, y: y + h, cursor: 'nwse-resize' },
      bc: { x: mx, y: y + h, cursor: 'ns-resize' },
      bl: { x: x, y: y + h, cursor: 'nesw-resize' },
      lc: { x: x, y: my, cursor: 'ew-resize' }
    };
  }

  function hitTestHandle(pt) {
    const handles = getHandles();
    for (const [key, h] of Object.entries(handles)) {
      if (Math.abs(pt.x - h.x) <= HANDLE_RADIUS + 4 && Math.abs(pt.y - h.y) <= HANDLE_RADIUS + 4) {
        return { name: key, cursor: h.cursor };
      }
    }
    return null;
  }

  function isInsideSelection(pt) {
    if (!sel) return false;
    return pt.x >= sel.x && pt.x <= sel.x + sel.w && pt.y >= sel.y && pt.y <= sel.y + sel.h;
  }

  // ---------------------------------------------------------------------------
  // Mask & Overlay Rendering
  // ---------------------------------------------------------------------------

  function renderMask() {
    const w = window.innerWidth;
    const h = window.innerHeight;
    maskCtx.clearRect(0, 0, w, h);

    if (!sel || sel.w <= 0 || sel.h <= 0) {
      // Darken whole screen slightly if no selection
      maskCtx.fillStyle = 'rgba(0, 0, 0, 0.25)';
      maskCtx.fillRect(0, 0, w, h);
      toolbar.style.display = 'none';
      subToolbar.style.display = 'none';
      sizeBadge.style.display = 'none';
      return;
    }

    // Outer darkened area
    maskCtx.fillStyle = 'rgba(0, 0, 0, 0.45)';
    // Top
    maskCtx.fillRect(0, 0, w, sel.y);
    // Bottom
    maskCtx.fillRect(0, sel.y + sel.h, w, h - (sel.y + sel.h));
    // Left
    maskCtx.fillRect(0, sel.y, sel.x, sel.h);
    // Right
    maskCtx.fillRect(sel.x + sel.w, sel.y, w - (sel.x + sel.w), sel.h);

    // Border around selection
    maskCtx.strokeStyle = '#0068ff';
    maskCtx.lineWidth = 2;
    maskCtx.strokeRect(sel.x, sel.y, sel.w, sel.h);

    // 8 resize handles
    const handles = getHandles();
    maskCtx.fillStyle = '#ffffff';
    maskCtx.strokeStyle = '#0068ff';
    maskCtx.lineWidth = 1.5;

    for (const h of Object.values(handles)) {
      maskCtx.beginPath();
      maskCtx.rect(h.x - HANDLE_RADIUS, h.y - HANDLE_RADIUS, HANDLE_RADIUS * 2, HANDLE_RADIUS * 2);
      maskCtx.fill();
      maskCtx.stroke();
    }

    // Dimension badge
    sizeBadge.textContent = `${Math.round(sel.w)} × ${Math.round(sel.h)}`;
    sizeBadge.style.display = 'block';
    const badgeTop = sel.y > 28 ? sel.y - 24 : sel.y + 6;
    const badgeLeft = Math.max(8, sel.x);
    sizeBadge.style.top = `${badgeTop}px`;
    sizeBadge.style.left = `${badgeLeft}px`;

    // Position toolbars
    positionToolbars();
  }

  function positionToolbars() {
    if (!sel || state === 'SELECTING' || sel.w < 20 || sel.h < 20) {
      toolbar.style.display = 'none';
      subToolbar.style.display = 'none';
      return;
    }

    toolbar.style.display = 'flex';
    const tbWidth = toolbar.offsetWidth || 390;
    const tbHeight = 38;

    let left = sel.x + sel.w - tbWidth;
    if (left < 10) left = 10;
    if (left + tbWidth > window.innerWidth - 10) left = window.innerWidth - tbWidth - 10;

    let top = sel.y + sel.h + 8;
    let placeAbove = false;
    if (top + tbHeight + 40 > window.innerHeight) {
      // Place above selection
      top = sel.y - tbHeight - 8;
      placeAbove = true;
      if (top < 10) top = sel.y + sel.h - tbHeight - 8; // fallback inside
    }

    toolbar.style.left = `${Math.round(left)}px`;
    toolbar.style.top = `${Math.round(top)}px`;

    // Sub-toolbar
    if (activeTool && activeTool !== 'mosaic') {
      subToolbar.style.display = 'flex';
      const subLeft = left;
      const subTop = placeAbove ? top - 40 : top + tbHeight + 6;
      subToolbar.style.left = `${Math.round(subLeft)}px`;
      subToolbar.style.top = `${Math.round(subTop)}px`;
    } else {
      subToolbar.style.display = 'none';
    }
  }

  // ---------------------------------------------------------------------------
  // Annotations & Drawing
  // ---------------------------------------------------------------------------

  function pushUndo() {
    undoStack.push(JSON.stringify(annotations));
    redoStack = [];
    updateUndoRedoBtns();
  }

  let redoStack = [];

  function undo() {
    if (annotations.length === 0) return;
    redoStack.push(JSON.stringify(annotations));
    if (undoStack.length > 0) {
      annotations = JSON.parse(undoStack.pop());
    } else {
      annotations = [];
    }
    renderAnnotations();
    updateUndoRedoBtns();
  }

  function redo() {
    if (redoStack.length === 0) return;
    undoStack.push(JSON.stringify(annotations));
    annotations = JSON.parse(redoStack.pop());
    renderAnnotations();
    updateUndoRedoBtns();
  }

  function updateUndoRedoBtns() {
    if (annotations.length > 0) {
      btnUndo.classList.remove('disabled');
    } else {
      btnUndo.classList.add('disabled');
    }
    if (redoStack.length > 0) {
      btnRedo.classList.remove('disabled');
    } else {
      btnRedo.classList.add('disabled');
    }
  }

  function renderAnnotations() {
    const w = window.innerWidth;
    const h = window.innerHeight;
    drawCtx.clearRect(0, 0, w, h);

    if (!sel) return;

    drawCtx.save();
    // Clip to selection box so annotations do not spill outside
    drawCtx.beginPath();
    drawCtx.rect(sel.x, sel.y, sel.w, sel.h);
    drawCtx.clip();

    // Render completed annotations
    for (const item of annotations) {
      drawAnnotationItem(drawCtx, item);
    }

    // Render currently active in-progress drawing
    if (currentDrawing) {
      drawAnnotationItem(drawCtx, currentDrawing);
    }

    drawCtx.restore();
  }

  function drawAnnotationItem(ctx, item) {
    ctx.save();
    ctx.strokeStyle = item.color || '#e53935';
    ctx.fillStyle = item.color || '#e53935';
    ctx.lineWidth = item.size || 4;
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';

    switch (item.type) {
      case 'rect': {
        const x = Math.min(item.x1, item.x2);
        const y = Math.min(item.y1, item.y2);
        const w = Math.abs(item.x2 - item.x1);
        const h = Math.abs(item.y2 - item.y1);
        ctx.strokeRect(x, y, w, h);
        break;
      }
      case 'circle': {
        const x = (item.x1 + item.x2) / 2;
        const y = (item.y1 + item.y2) / 2;
        const rx = Math.abs(item.x2 - item.x1) / 2;
        const ry = Math.abs(item.y2 - item.y1) / 2;
        ctx.beginPath();
        ctx.ellipse(x, y, rx, ry, 0, 0, 2 * Math.PI);
        ctx.stroke();
        break;
      }
      case 'line': {
        ctx.beginPath();
        ctx.moveTo(item.x1, item.y1);
        ctx.lineTo(item.x2, item.y2);
        ctx.stroke();
        break;
      }
      case 'arrow': {
        drawArrow(ctx, item.x1, item.y1, item.x2, item.y2, item.size);
        break;
      }
      case 'doodle': {
        if (item.points && item.points.length > 1) {
          ctx.beginPath();
          ctx.moveTo(item.points[0].x, item.points[0].y);
          for (let i = 1; i < item.points.length; i++) {
            ctx.lineTo(item.points[i].x, item.points[i].y);
          }
          ctx.stroke();
        }
        break;
      }
      case 'mosaic': {
        applyMosaic(ctx, item);
        break;
      }
      case 'text': {
        ctx.font = `600 ${item.fontSize || 16}px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif`;
        const lines = (item.text || '').split('\n');
        const lineHeight = (item.fontSize || 16) * 1.3;
        lines.forEach((line, idx) => {
          ctx.fillText(line, item.x, item.y + idx * lineHeight);
        });
        break;
      }
    }
    ctx.restore();
  }

  function drawArrow(ctx, fromX, fromY, toX, toY, size) {
    const headLen = Math.max(12, size * 3.5);
    const angle = Math.atan2(toY - fromY, toX - fromX);

    // Main line
    ctx.beginPath();
    ctx.moveTo(fromX, fromY);
    ctx.lineTo(toX, toY);
    ctx.stroke();

    // Arrowhead
    ctx.beginPath();
    ctx.moveTo(toX, toY);
    ctx.lineTo(toX - headLen * Math.cos(angle - Math.PI / 7), toY - headLen * Math.sin(angle - Math.PI / 7));
    ctx.lineTo(toX - headLen * Math.cos(angle + Math.PI / 7), toY - headLen * Math.sin(angle + Math.PI / 7));
    ctx.closePath();
    ctx.fill();
  }

  function applyMosaic(ctx, item) {
    if (!item.points || item.points.length === 0) return;
    const blockSize = 12;

    for (const pt of item.points) {
      const radius = 20;
      const startX = Math.floor((pt.x - radius) / blockSize) * blockSize;
      const startY = Math.floor((pt.y - radius) / blockSize) * blockSize;
      const endX = Math.ceil((pt.x + radius) / blockSize) * blockSize;
      const endY = Math.ceil((pt.y + radius) / blockSize) * blockSize;

      for (let bx = startX; bx < endX; bx += blockSize) {
        for (let by = startY; by < endY; by += blockSize) {
          // Sample pixel color from baseCanvas at center of block
          const sampleX = Math.min(Math.max(0, Math.floor((bx + blockSize / 2) * dpr)), baseCanvas.width - 1);
          const sampleY = Math.min(Math.max(0, Math.floor((by + blockSize / 2) * dpr)), baseCanvas.height - 1);
          try {
            const pixel = baseCtx.getImageData(sampleX, sampleY, 1, 1).data;
            ctx.fillStyle = `rgb(${pixel[0]}, ${pixel[1]}, ${pixel[2]})`;
            ctx.fillRect(bx, by, blockSize, blockSize);
          } catch (_) {}
        }
      }
    }
  }

  // ---------------------------------------------------------------------------
  // Magnifier Loupe
  // ---------------------------------------------------------------------------

  function updateLoupe(e) {
    if (!imageLoaded) return;
    if (state !== 'NONE' && state !== 'SELECTING') {
      loupe.style.display = 'none';
      return;
    }

    loupe.style.display = 'block';

    let lx = e.clientX + 16;
    let ly = e.clientY + 16;
    if (lx + 120 > window.innerWidth) lx = e.clientX - 130;
    if (ly + 120 > window.innerHeight) ly = e.clientY - 130;

    loupe.style.left = `${lx}px`;
    loupe.style.top = `${ly}px`;

    // Draw magnified pixels (9x9 pixels blown up onto 110x110)
    const srcX = Math.round(e.clientX * dpr) - 5;
    const srcY = Math.round(e.clientY * dpr) - 5;

    loupeCtx.imageSmoothingEnabled = false;
    loupeCtx.clearRect(0, 0, 110, 110);
    try {
      loupeCtx.drawImage(baseCanvas, srcX, srcY, 11, 11, 0, 0, 110, 110);
    } catch (_) {}

    // Draw crosshair on loupe
    loupeCtx.strokeStyle = '#0068ff';
    loupeCtx.lineWidth = 1;
    loupeCtx.strokeRect(50, 50, 10, 10);

    // RGB & coord string
    try {
      const p = baseCtx.getImageData(Math.round(e.clientX * dpr), Math.round(e.clientY * dpr), 1, 1).data;
      const hex = `#${((1 << 24) + (p[0] << 16) + (p[1] << 8) + p[2]).toString(16).slice(1).toUpperCase()}`;
      loupeInfo.textContent = `${e.clientX}, ${e.clientY} | ${hex}`;
    } catch (_) {
      loupeInfo.textContent = `${e.clientX}, ${e.clientY}`;
    }
  }

  // ---------------------------------------------------------------------------
  // Mouse & Interaction Handlers
  // ---------------------------------------------------------------------------

  window.addEventListener('mousemove', (e) => {
    updateLoupe(e);

    const pt = { x: e.clientX, y: e.clientY };

    if (state === 'SELECTING') {
      const x = Math.min(startPt.x, pt.x);
      const y = Math.min(startPt.y, pt.y);
      const w = Math.abs(pt.x - startPt.x);
      const h = Math.abs(pt.y - startPt.y);
      sel = { x, y, w, h };
      renderMask();
      return;
    }

    if (state === 'MOVING') {
      let nx = pt.x - moveOffset.x;
      let ny = pt.y - moveOffset.y;
      nx = Math.max(0, Math.min(nx, window.innerWidth - sel.w));
      ny = Math.max(0, Math.min(ny, window.innerHeight - sel.h));
      sel.x = nx;
      sel.y = ny;
      renderMask();
      renderAnnotations();
      return;
    }

    if (state === 'RESIZING') {
      resizeHandle(pt);
      renderMask();
      renderAnnotations();
      return;
    }

    if (state === 'DRAWING') {
      if (currentDrawing) {
        if (currentDrawing.type === 'doodle' || currentDrawing.type === 'mosaic') {
          currentDrawing.points.push(pt);
        } else {
          currentDrawing.x2 = pt.x;
          currentDrawing.y2 = pt.y;
        }
        renderAnnotations();
      }
      return;
    }

    // Hover cursor management
    if (state === 'SELECTED') {
      if (activeTool) {
        document.body.style.cursor = 'crosshair';
        return;
      }
      const handle = hitTestHandle(pt);
      if (handle) {
        document.body.style.cursor = handle.cursor;
      } else if (isInsideSelection(pt)) {
        document.body.style.cursor = 'move';
      } else {
        document.body.style.cursor = 'crosshair';
      }
    } else {
      document.body.style.cursor = 'crosshair';
    }
  });

  window.addEventListener('mousedown', (e) => {
    if (e.button === 2) return; // Right click handled by contextmenu
    if (e.target.closest('#toolbar') || e.target.closest('#sub-toolbar') || e.target === textEditor) {
      return;
    }

    const pt = { x: e.clientX, y: e.clientY };
    startPt = pt;

    // If text editor is open, commit text
    if (textEditor.style.display === 'block') {
      commitTextEditor();
    }

    // If no selection yet, start selecting
    if (state === 'NONE') {
      state = 'SELECTING';
      sel = { x: pt.x, y: pt.y, w: 0, h: 0 };
      hintTooltip.style.opacity = '0';
      return;
    }

    // If selection already exists
    if (state === 'SELECTED') {
      if (activeTool) {
        // Start drawing annotation if inside or on selection
        if (isInsideSelection(pt)) {
          if (activeTool === 'text') {
            openTextEditor(pt);
            return;
          }
          pushUndo();
          state = 'DRAWING';
          const size = strokeWidth;
          const color = strokeColor;

          if (activeTool === 'doodle' || activeTool === 'mosaic') {
            currentDrawing = { type: activeTool, points: [pt], color, size };
          } else {
            currentDrawing = { type: activeTool, x1: pt.x, y1: pt.y, x2: pt.x, y2: pt.y, color, size };
          }
          renderAnnotations();
        }
        return;
      }

      // Check handles
      const handle = hitTestHandle(pt);
      if (handle) {
        state = 'RESIZING';
        activeHandle = handle.name;
        return;
      }

      // Check moving selection
      if (isInsideSelection(pt)) {
        state = 'MOVING';
        moveOffset = { x: pt.x - sel.x, y: pt.y - sel.y };
        return;
      }

      // Clicked outside selection without a tool -> reset selection to new drag
      state = 'SELECTING';
      annotations = [];
      undoStack = [];
      redoStack = [];
      updateUndoRedoBtns();
      sel = { x: pt.x, y: pt.y, w: 0, h: 0 };
      renderMask();
      renderAnnotations();
    }
  });

  window.addEventListener('mouseup', (e) => {
    if (e.target.closest('#toolbar') || e.target.closest('#sub-toolbar')) return;

    if (state === 'SELECTING') {
      if (sel && sel.w > 10 && sel.h > 10) {
        state = 'SELECTED';
      } else {
        sel = null;
        state = 'NONE';
        hintTooltip.style.opacity = '1';
      }
      renderMask();
      return;
    }

    if (state === 'MOVING' || state === 'RESIZING') {
      state = 'SELECTED';
      renderMask();
      return;
    }

    if (state === 'DRAWING') {
      if (currentDrawing) {
        annotations.push(currentDrawing);
        currentDrawing = null;
        updateUndoRedoBtns();
      }
      state = 'SELECTED';
      renderAnnotations();
      return;
    }
  });

  function resizeHandle(pt) {
    if (!sel || !activeHandle) return;
    const { x, y, w, h } = sel;
    const r = x + w;
    const b = y + h;

    switch (activeHandle) {
      case 'tl':
        sel.x = Math.min(pt.x, r - 10);
        sel.y = Math.min(pt.y, b - 10);
        sel.w = r - sel.x;
        sel.h = b - sel.y;
        break;
      case 'tc':
        sel.y = Math.min(pt.y, b - 10);
        sel.h = b - sel.y;
        break;
      case 'tr':
        sel.w = Math.max(10, pt.x - x);
        sel.y = Math.min(pt.y, b - 10);
        sel.h = b - sel.y;
        break;
      case 'rc':
        sel.w = Math.max(10, pt.x - x);
        break;
      case 'br':
        sel.w = Math.max(10, pt.x - x);
        sel.h = Math.max(10, pt.y - y);
        break;
      case 'bc':
        sel.h = Math.max(10, pt.y - y);
        break;
      case 'bl':
        sel.x = Math.min(pt.x, r - 10);
        sel.w = r - sel.x;
        sel.h = Math.max(10, pt.y - y);
        break;
      case 'lc':
        sel.x = Math.min(pt.x, r - 10);
        sel.w = r - sel.x;
        break;
    }
  }

  // Double click inside selection -> Done / Accept immediately!
  window.addEventListener('dblclick', (e) => {
    if (e.target.closest('#toolbar') || e.target.closest('#sub-toolbar')) return;
    if (sel && isInsideSelection({ x: e.clientX, y: e.clientY })) {
      finishCapture('accept');
    }
  });

  // Right click -> cancel tool or clear selection or exit
  window.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    if (activeTool) {
      setActiveTool(null);
      return;
    }
    if (sel) {
      sel = null;
      state = 'NONE';
      annotations = [];
      undoStack = [];
      redoStack = [];
      updateUndoRedoBtns();
      hintTooltip.style.opacity = '1';
      renderMask();
      renderAnnotations();
      return;
    }
    finishCapture('cancel');
  });

  // ---------------------------------------------------------------------------
  // Text Tool Overlay
  // ---------------------------------------------------------------------------

  function openTextEditor(pt) {
    textEditor.style.left = `${pt.x}px`;
    textEditor.style.top = `${pt.y}px`;
    textEditor.style.color = strokeColor;
    textEditor.style.fontSize = `${Math.max(14, strokeWidth * 4)}px`;
    textEditor.value = '';
    textEditor.style.display = 'block';
    textEditor.focus();
    textEditor.dataset.x = pt.x;
    textEditor.dataset.y = pt.y;
  }

  function commitTextEditor() {
    if (textEditor.style.display !== 'block') return;
    const text = textEditor.value.trim();
    const x = parseFloat(textEditor.dataset.x);
    const y = parseFloat(textEditor.dataset.y) + 16;
    if (text) {
      pushUndo();
      annotations.push({
        type: 'text',
        x,
        y,
        text,
        color: strokeColor,
        fontSize: Math.max(14, strokeWidth * 4)
      });
      renderAnnotations();
      updateUndoRedoBtns();
    }
    textEditor.style.display = 'none';
  }

  textEditor.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      commitTextEditor();
    } else if (e.key === 'Escape') {
      textEditor.style.display = 'none';
    }
  });

  // ---------------------------------------------------------------------------
  // Toolbar Buttons & Controls
  // ---------------------------------------------------------------------------

  function setActiveTool(tool) {
    if (activeTool === tool) {
      activeTool = null;
    } else {
      activeTool = tool;
    }

    document.querySelectorAll('#toolbar .tool-btn[data-tool]').forEach((b) => {
      if (b.dataset.tool === activeTool) {
        b.classList.add('active');
      } else {
        b.classList.remove('active');
      }
    });

    positionToolbars();
  }

  document.querySelectorAll('#toolbar .tool-btn[data-tool]').forEach((btn) => {
    btn.addEventListener('click', () => {
      setActiveTool(btn.dataset.tool);
    });
  });

  // Size picker
  document.querySelectorAll('.size-dot').forEach((dot) => {
    dot.addEventListener('click', () => {
      document.querySelectorAll('.size-dot').forEach((d) => d.classList.remove('active'));
      dot.classList.add('active');
      const s = dot.dataset.size;
      if (s === 'small') strokeWidth = 2;
      else if (s === 'medium') strokeWidth = 4;
      else if (s === 'large') strokeWidth = 8;
    });
  });

  // Color picker
  document.querySelectorAll('.color-swatch').forEach((swatch) => {
    swatch.addEventListener('click', () => {
      document.querySelectorAll('.color-swatch').forEach((s) => s.classList.remove('active'));
      swatch.classList.add('active');
      strokeColor = swatch.dataset.color;
    });
  });

  // Action buttons
  btnUndo.addEventListener('click', undo);
  btnRedo.addEventListener('click', redo);

  btnAccept.addEventListener('click', () => finishCapture('accept'));
  btnCopy.addEventListener('click', () => finishCapture('copy'));
  btnSave.addEventListener('click', () => finishCapture('save'));
  btnCancel.addEventListener('click', () => finishCapture('cancel'));

  // Keyboard Shortcuts
  window.addEventListener('keydown', (e) => {
    if (textEditor.style.display === 'block') return;

    if (e.key === 'Escape') {
      if (activeTool) {
        setActiveTool(null);
        return;
      }
      finishCapture('cancel');
    } else if (e.key === 'Enter') {
      if (sel) finishCapture('accept');
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'c') {
      if (sel) finishCapture('copy');
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
      if (sel) finishCapture('save');
    } else if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z') {
      undo();
    } else if ((e.ctrlKey || e.metaKey) && (e.key.toLowerCase() === 'y' || (e.shiftKey && e.key.toLowerCase() === 'z'))) {
      redo();
    }
  });

  // ---------------------------------------------------------------------------
  // Export & Finalization
  // ---------------------------------------------------------------------------

  function finishCapture(action) {
    if (action === 'cancel' || !sel || sel.w <= 0 || sel.h <= 0) {
      if (window.screencap && window.screencap.cancel) {
        window.screencap.cancel();
      }
      return;
    }

    // Render cropped image with annotations to offscreen canvas
    const cropW = Math.round(sel.w * dpr);
    const cropH = Math.round(sel.h * dpr);
    const offCanvas = document.createElement('canvas');
    offCanvas.width = cropW;
    offCanvas.height = cropH;
    const offCtx = offCanvas.getContext('2d');

    const sx = Math.round(sel.x * dpr);
    const sy = Math.round(sel.y * dpr);

    // Draw base canvas
    offCtx.drawImage(baseCanvas, sx, sy, cropW, cropH, 0, 0, cropW, cropH);

    // Draw annotations canvas
    offCtx.drawImage(drawCanvas, sx, sy, cropW, cropH, 0, 0, cropW, cropH);

    const dataUrl = offCanvas.toDataURL('image/png');

    if (window.screencap && window.screencap[action]) {
      window.screencap[action](dataUrl);
    }
  }
})();
