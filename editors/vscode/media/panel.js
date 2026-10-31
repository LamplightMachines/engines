// The live panel's webview (ADR 0040; design/vs_extension/SPEC.md): renders the PanelModel the
// extension posts (≤ 15 Hz) and sends clicks back — `{ type: 'cmd', name, arg }`, `{ type: 'reveal',
// path, label }` (jump-to-source), `{ type: 'sections', open }`. The skeleton is built once; lists are
// rebuilt only when their shape changes, so a click never lands on a replaced button; values update
// in place. The meters' peak hold and readout smoothing live here (they're display, not state).
// @ts-check
(function () {
  // @ts-ignore — provided by VS Code
  const vscode = acquireVsCodeApi();
  const $ = (/** @type {string} */ id) => /** @type {HTMLElement} */ (document.getElementById(id));
  const shapes = /** @type {Record<string, string>} */ ({});

  // ── Icons (from the design's mockup): 24-unit strokes, `currentColor` ────────────────────────
  const ICONS = /** @type {Record<string, string>} */ ({
    wave: '<path d="M2 12h2"/><path d="M6 8v8"/><path d="M10 4v16"/><path d="M14 7v10"/><path d="M18 10v4"/><path d="M22 12h-2"/>',
    speaker: '<path d="M11 5 6 9H3v6h3l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.5 5.5a9 9 0 0 1 0 13"/>',
    speakerX: '<path d="M11 5 6 9H3v6h3l5 4z"/><path d="m22 9-6 6"/><path d="m16 9 6 6"/>',
    stems: '<path d="M2 6c1.3-1.6 2.7-1.6 4 0s2.7 1.6 4 0 2.7-1.6 4 0"/><path d="M2 12c1.3-1.6 2.7-1.6 4 0s2.7 1.6 4 0"/><path d="M2 18c1.3-1.6 2.7-1.6 4 0s2.7 1.6 4 0"/><path d="M19 8v12"/><path d="m15.5 16.5 3.5 3.5 3.5-3.5"/>',
    eject: '<path d="M5 15h14L12 6z"/><path d="M5 19h14"/>',
    play: '<path d="M7 4.5v15a1 1 0 0 0 1.5.86l12.4-7.5a1 1 0 0 0 0-1.72L8.5 3.64A1 1 0 0 0 7 4.5z" fill="currentColor" stroke="none"/>',
    stop: '<rect x="4" y="4" width="16" height="16" rx="2" fill="currentColor" stroke="none"/>',
    restart: '<path d="M3 12a9 9 0 1 0 2.64-6.36L3 8"/><path d="M3 3v5h5"/>',
    hold: '<path d="M12 17v5"/><path d="M8 3h8l-1 6 3 4H6l3-4z"/>',
    drift: '<path d="M2 9c2-2 4-2 6 0s4 2 6 0 4-2 6 0"/><path d="M2 15c2-2 4-2 6 0s4 2 6 0 4-2 6 0"/>',
    orrery: '<circle cx="12" cy="12" r="2.5"/><ellipse cx="12" cy="12" rx="10" ry="4.5"/><circle cx="20" cy="9.3" r="1.6" fill="currentColor"/>',
    limiter: '<path d="M2 5h20"/><path d="M2 15l3-7 3 10 3-12 3 12 3-9 3 6 2-3"/>',
    chevron: '<path d="m6 15 6-6 6 6"/>',
    headphones: '<path d="M3 15v-3a9 9 0 0 1 18 0v3"/><path d="M3 15h4v5H5a2 2 0 0 1-2-2z"/><path d="M21 15h-4v5h2a2 2 0 0 0 2-2z"/>',
    back: '<path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-4"/>',
    loop: '<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>',
    diff: '<circle cx="18" cy="18" r="3"/><circle cx="6" cy="6" r="3"/><path d="M13 6h3a2 2 0 0 1 2 2v7"/><path d="M11 18H8a2 2 0 0 1-2-2V9"/>',
    revert: '<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>',
  });

  /** A companion's icon: SVG path data in a 24×24 box, set as an attribute — never parsed as
   *  markup, so the panel's security policy holds whatever a companion sends. */
  function pathIcon(/** @type {string} */ d, size = 12, stroke = 2) {
    const ns = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(ns, 'svg');
    for (const [k, v] of Object.entries({ width: size, height: size, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': stroke, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true' })) {
      svg.setAttribute(k, String(v));
    }
    const path = document.createElementNS(ns, 'path');
    path.setAttribute('d', d);
    svg.append(path);
    return svg;
  }

  /** An inline SVG icon (constant markup — no data goes into it). */
  function icon(/** @type {string} */ name, size = 12, stroke = 2) {
    const t = document.createElement('template');
    t.innerHTML = `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="${stroke}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[name]}</svg>`;
    return /** @type {SVGElement} */ (t.content.firstChild);
  }

  function withClass(/** @type {Element} */ e, /** @type {string} */ cls) {
    e.classList.add(cls);
    return e;
  }

  /** @param {string} tag @param {Record<string, any>} [attrs] @param {(Node|string|null|undefined|false)[]} [kids] */
  function el(tag, attrs = {}, kids = []) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'class') e.className = v;
      else if (k === 'text') e.textContent = v;
      else if (k === 'style') e.style.cssText = v; // CSSOM — the CSP forbids style attributes
      else e.setAttribute(k, v === true ? '' : String(v));
    }
    for (const kid of kids) if (kid) e.append(kid);
    return e;
  }

  /** Rebuild `container` with `build()` only when `shape` changed. */
  function keyed(/** @type {HTMLElement} */ container, /** @type {string} */ key, /** @type {string} */ shape, /** @type {() => Node[]} */ build) {
    if (shapes[key] === shape) return false;
    shapes[key] = shape;
    container.replaceChildren(...build());
    return true;
  }

  const fmtDb = (/** @type {number | null} */ d) => {
    if (d === null || d <= -119) return '−∞';
    const t = d.toFixed(1);
    return t === '-0.0' ? '0.0' : t.replace('-', '−');
  };
  const pct = (/** @type {number} */ db) => Math.max(0, Math.min(100, ((db + 60) / 60) * 100));
  const setText = (/** @type {HTMLElement} */ e, /** @type {string} */ t) => {
    if (e.textContent !== t) e.textContent = t;
  };
  const press = (/** @type {Element} */ e, /** @type {boolean} */ on) => e.setAttribute('aria-pressed', String(on));

  // ── Sections: open/closed per user (the extension keeps a copy across panel closes) ────────
  const SECTIONS = /** @type {const} */ (['h', 'i', 's', 'r', 'a']);
  const saved = vscode.getState() ?? {};
  /** @type {Record<string, boolean>} */
  let open = { h: true, i: true, s: true, r: true, a: false, ...(saved.open ?? {}) };

  function applySections() {
    for (const k of SECTIONS) {
      const sec = $(`sec-${k}`);
      sec.classList.toggle('open', !!open[k]);
      const t = /** @type {HTMLElement} */ (sec.querySelector('.sec-toggle'));
      t.setAttribute('aria-expanded', String(!!open[k]));
      t.title = `${open[k] ? 'Collapse' : 'Expand'} ${t.dataset.label}`;
    }
  }

  // ── The skeleton ─────────────────────────────────────────────────────────────────────────────
  /** A collapsible section: header button (chevron, title, collapsed summary) + right-side controls. */
  function section(/** @type {string} */ key, /** @type {string} */ label, /** @type {Node[]} */ right, /** @type {HTMLElement} */ body) {
    const wrap = el('div', { class: 'sec-body', id: `b-${key}` }, [body]);
    return el('section', { class: 'sec', id: `sec-${key}`, 'aria-labelledby': `h-${key}` }, [
      el('div', { class: 'sec-head' }, [
        el('h2', { id: `h-${key}` }, [
          el('button', { class: 'sec-toggle', 'data-toggle': key, 'data-label': label, 'aria-controls': `b-${key}` }, [
            withClass(icon('chevron', 10, 2.6), 'chev'),
            label,
            el('span', { class: 'summary', id: `sum-${key}` }),
          ]),
        ]),
        ...right,
      ]),
      wrap,
    ]);
  }

  function build() {
    const app = $('app');
    const ticks = el('div', { class: 'timeline', 'aria-hidden': 'true', id: 'timeline' });
    const tensionSegs = el('div', { class: 'segs', id: 'tensionSegs', role: 'img', 'aria-label': 'Tension' }, [...Array(12)].map(() => el('span', { class: 'seg-t' })));
    const scaleMarks = el('div', { class: 'scale', 'aria-hidden': 'true' }, [0, -12, -24, -36, -48, -60].map((d, i) => el('span', { text: d === 0 ? '0' : `−${-d}`, style: `top: ${i * 20}%` })));
    app.replaceChildren(
      el('header', { class: 'hdr' }, [
        el('div', { class: 'tile', 'aria-hidden': 'true' }, [icon('wave', 20)]),
        el('div', { class: 'meta' }, [
          el('div', { class: 'line1' }, [
            el('h1', { id: 'piece', text: 'No piece playing' }),
            el('span', { class: 'chip-rev', id: 'revChip', title: 'Revision' }, [
              el('span', { id: 'rev' }),
              el('span', { class: 'unsaved', id: 'unsaved', title: 'The editor has edits the player hasn’t heard — save (⌘S) to hear them', role: 'img', 'aria-label': 'Unsaved edits', hidden: true }),
            ]),
            el('span', { class: 'pill', id: 'pill' }, [el('span', { class: 'dot', 'aria-hidden': 'true' }), el('span', { id: 'pillText' })]),
          ]),
          el('button', { class: 'line2 devbtn', id: 'deviceLine', 'data-cmd': 'chooseOutput', title: 'Play on another device… (headphones, a loopback into Live)' }, [icon('speaker', 13), el('span', { id: 'device' }), withClass(icon('chevron', 9, 2.6), 'devchev')]),
        ]),
        el('button', { class: 'hbtn', id: 'previewBtn', 'data-cmd': 'preview', 'aria-label': 'Render a preview (mix or stems)…', title: 'Render a preview (mix or stems)…' }, [icon('stems', 14, 1.9)]),
        el('button', { class: 'hbtn', id: 'closeBtn', 'data-cmd': 'quit', 'aria-label': 'Close the piece', title: 'Close the piece (the player quits and frees the audio device)' }, [icon('eject', 13, 1.9)]),
      ]),
      el('div', { class: 'message', id: 'message', role: 'alert', hidden: true }),
      el('div', { class: 'idle', id: 'idle', hidden: true }, [
        el('p', {}, ['Open a piece doc (', el('code', { text: 'pieces/<slug>/<slug>.json5' }), ') and play it — each save (⌘S) is heard mid-loop, about 0.2 s later.']),
        el('button', { class: 'primary', 'data-cmd': 'start' }, [icon('play', 11), 'Play this piece']),
      ]),
      el('div', { id: 'live', hidden: true }, [
        el('div', { class: 'transport' }, [
          el('div', { class: 'group', role: 'group', 'aria-label': 'Transport' }, [
            el('button', { class: 'tbtn', id: 'play', 'data-cmd': 'play', 'aria-label': 'Play', title: 'Play' }, [icon('play', 11)]),
            el('button', { class: 'tbtn', 'data-cmd': 'pause', 'aria-label': 'Stop', title: 'Stop (the player stays open)' }, [icon('stop', 10)]),
            el('button', { class: 'tbtn', 'data-cmd': 'restart', 'aria-label': 'Restart', title: 'Restart from the top of the loop (a fresh controller: the drift starts over)' }, [icon('restart', 9, 2.2)]),
          ]),
          el('div', { class: 'divider', 'aria-hidden': 'true' }),
          el('div', { class: 'seg', role: 'group', 'aria-label': 'Harmony mode' }, [
            el('button', { id: 'hold', 'data-cmd': 'hold', title: 'Hold: play exactly what’s written (for fine-tuning)' }, [icon('hold', 11, 2.2), 'Hold']),
            el('button', { id: 'drift', 'data-cmd': 'drift', title: 'Drift: let the controller wander' }, [icon('drift', 11, 2.2), 'Drift']),
          ]),
          el('div', { class: 'divider', 'aria-hidden': 'true' }),
          el('button', { class: 'toggle', id: 'orrery', 'data-cmd': 'orrery', title: 'Orrery: generative harmony' }, [icon('orrery', 12), 'Orrery']),
          el('button', { class: 'toggle', id: 'limiter', 'data-cmd': 'limiter', title: 'Limiter: the live-only protection limiter on the speakers — never in renders (ADR 0021)' }, [icon('limiter', 12), 'Limiter']),
          el('span', { class: 'gr', id: 'gr', title: 'Limiter gain reduction since the last update', hidden: true }),
          el('span', { class: 'underruns', id: 'underruns', hidden: true }),
        ]),
        el('div', { class: 'refused', id: 'refused', role: 'alert', hidden: true }),
        el('div', { class: 'loopread' }, [
          el('div', { class: 'counter inset' }, [el('div', { class: 'lbl', text: 'Loop' }), el('div', { class: 'num', id: 'loopNum', text: '0' })]),
          el('div', { class: 'position inset' }, [
            el('div', { class: 'top' }, [el('span', { class: 'lbl', text: 'Loop position' }), el('span', { class: 't' }, [el('span', { id: 'posNow' }), el('span', { class: 'of', id: 'posOf' })])]),
            ticks,
          ]),
        ]),
        section(
          'h',
          'Harmony',
          [el('span', { class: 'sec-label', text: 'Scale' }), el('span', { class: 'chip', id: 'scaleRoot' }), el('span', { class: 'chip', id: 'scaleMode' })],
          el('div', { class: 'harmony' }, [
            el('div', { class: 'chordrow' }, [
              el('button', { class: 'jump', id: 'chordJump', 'data-reveal': JSON.stringify(['harmony']), title: 'Go to harmony in the doc' }, [el('span', { class: 'jt chord', id: 'chord' }), el('span', { class: 'tones', id: 'tones' })]),
              el('span', { class: 'spacer' }),
              el('div', { class: 'tension', id: 'tension' }, [el('span', { class: 'sec-label', text: 'Tension' }), tensionSegs]),
            ]),
            el('div', { class: 'keyboard', id: 'keyboard', role: 'img' }),
            el('div', { class: 'legend' }, [
              el('span', {}, [el('span', { class: 'sw-scale', 'aria-hidden': 'true' }), 'In scale']),
              el('span', {}, [el('span', { class: 'sw-chord', 'aria-hidden': 'true' }), 'Chord tone']),
              el('span', {}, [el('span', { class: 'sw-pad', 'aria-hidden': 'true' }), 'Pad']),
              el('span', {}, [el('span', { class: 'sw-bass', 'aria-hidden': 'true' }), 'Bass']),
            ]),
          ]),
        ),
        section(
          'i',
          'Instruments',
          [el('button', { class: 'link', id: 'clearSolo', 'data-cmd': 'clearSolo', title: 'Unsolo every instrument', hidden: true, text: 'clear solo' })],
          el('div', { class: 'mixer' }, [
            scaleMarks,
            el('div', { class: 'strips', id: 'strips' }),
            el('div', { class: 'vdiv', 'aria-hidden': 'true' }),
            el('div', { class: 'strip', id: 'master' }, [
              el('button', { class: 'jump namebar', 'data-reveal': JSON.stringify(['global']), title: 'Go to global in the doc', style: 'background: var(--engines-track-master)' }, [el('span', { class: 'jt jt-dark', text: 'Master' })]),
              meterWell(),
              el('div', { class: 'readout', title: 'Master level, dB' }),
              el('div', { class: 'peakbox', title: 'Loop peak, dBFS' }, [el('span', { text: 'pk' }), el('span', { class: 'v', id: 'loopPeak', text: '−∞' })]),
            ]),
          ]),
        ),
        section(
          's',
          'Song',
          [el('button', { class: 'back', id: 'back', 'data-cmd': 'songStop' }, [icon('back', 9, 2.4), 'Back to loop'])],
          el('div', { class: 'song' }, [el('div', { class: 'structure', id: 'structure', role: 'img' }), el('div', { class: 'blocks', id: 'blocks' })]),
        ),
        section(
          'r',
          'Revisions',
          [el('button', { class: 'link', id: 'allRevs', 'data-cmd': 'revisions', title: 'Every revision (revert picker)', text: 'All…', hidden: true })],
          el('div', { class: 'revs', id: 'revs' }),
        ),
        section(
          'a',
          'Activity',
          [
            el('button', { class: 'link', id: 'openPreview', 'data-cmd': 'openPreview', text: 'open the preview', hidden: true }),
            el('button', { class: 'link', id: 'revealStems', 'data-cmd': 'revealStems', text: 'stems folder', hidden: true }),
            el('button', { class: 'link', 'data-cmd': 'output', text: 'output' }),
            el('span', { class: 'menu', id: 'menu' }),
          ],
          el('div', { class: 'activity' }, [el('div', { class: 'warn-line', id: 'warnings' }), el('div', { id: 'activityItems' })]),
        ),
      ]),
    );
    applySections();
  }

  /** Two meter bars (L/R) in a well: each a clip box over a full-height gradient, plus a peak line. */
  function meterWell() {
    const bar = (/** @type {string} */ side) => el('div', { class: `bar ${side}` }, [el('div', { class: 'lit' }, [el('div', { class: 'grad' })]), el('div', { class: 'pk' })]);
    return el('div', { class: 'well', 'aria-hidden': 'true' }, [bar('l'), bar('r')]);
  }

  // ── Meters: peak hold (~1 s, then falling 20 dB/s) and readouts at ~4 Hz ───────────────────
  /** @type {Map<string, { l: number, r: number, lAt: number, rAt: number, last: number, shownAt: number }>} */
  const holds = new Map();
  function meter(/** @type {HTMLElement} */ strip, /** @type {string} */ key, /** @type {number} */ lDb, /** @type {number} */ rDb, /** @type {boolean} */ silent) {
    const now = performance.now();
    const h = holds.get(key) ?? { l: -120, r: -120, lAt: 0, rAt: 0, last: now, shownAt: 0 };
    const dt = (now - h.last) / 1000;
    h.last = now;
    const fall = (/** @type {number} */ hold, /** @type {number} */ at, /** @type {number} */ v) => (v >= hold ? [v, now] : now - at > 1000 ? [Math.max(v, hold - 20 * dt), at] : [hold, at]);
    if (silent) {
      lDb = rDb = -120;
    }
    [h.l, h.lAt] = fall(h.l, h.lAt, lDb);
    [h.r, h.rAt] = fall(h.r, h.rAt, rDb);
    holds.set(key, h);
    const bars = strip.querySelectorAll('.bar');
    [[lDb, h.l], [rDb, h.r]].forEach(([v, hold], i) => {
      const b = /** @type {HTMLElement} */ (bars[i]);
      /** @type {HTMLElement} */ (b.querySelector('.lit')).style.height = `${pct(v).toFixed(1)}%`;
      const pk = /** @type {HTMLElement} */ (b.querySelector('.pk'));
      pk.style.bottom = `${pct(hold).toFixed(1)}%`;
      pk.hidden = hold <= -60;
    });
    const readout = /** @type {HTMLElement} */ (strip.querySelector('.readout'));
    if (silent || now - h.shownAt > 250) {
      h.shownAt = now;
      setText(readout, fmtDb(silent ? null : Math.max(h.l, h.r)));
    }
    return silent ? null : Math.max(h.l, h.r);
  }

  // ── Lists ────────────────────────────────────────────────────────────────────────────────────
  function strip(/** @type {any} */ s) {
    return el('div', { class: 'strip', 'data-id': s.id }, [
      el('button', { class: 'jump namebar', 'data-reveal': JSON.stringify(s.path), title: `Go to instruments.${s.id} in the doc`, style: `background: ${s.color}` }, [el('span', { class: 'jt jt-dark', text: s.id })]),
      meterWell(),
      el('div', { class: 'readout', title: 'Level, dB' }),
      el('div', { class: 'ms' }, [
        el('button', { class: 'mute', 'data-cmd': 'mute', 'data-arg': s.id }, [icon('speaker', 11, 2.2)]),
        el('button', { class: 'solo', 'data-cmd': 'solo', 'data-arg': s.id }, [icon('headphones', 11, 2.2)]),
        el('span', { class: 'acts' }),
      ]),
    ]);
  }

  function keyboard(/** @type {any} */ kb) {
    const whites = kb.cells.filter((/** @type {any} */ c) => !c.black);
    const w = 100 / whites.length;
    const label = (/** @type {any} */ c) => (c.pad || c.bass || c.root || (!c.black && c.pc === 0) ? c.name.replace('#', '♯').replace(/^([A-G])b/, '$1♭') : '');
    const marks = (/** @type {any} */ c) => [c.chord && el('span', { class: 'cap' }), c.pad && el('span', { class: 'pad' }), c.bass && el('span', { class: 'bass' }), label(c) && el('span', { class: 'lbl', text: label(c) })];
    const cls = (/** @type {any} */ c) => ['key', c.black ? 'black' : 'white', c.inScale && 'in', c.root && 'root'].filter(Boolean).join(' ');
    const out = [el('div', { class: 'whites' }, whites.map((/** @type {any} */ c) => el('div', { class: cls(c) }, marks(c))))];
    let wi = 0;
    for (const c of kb.cells) {
      if (c.black) out.push(el('div', { class: cls(c), style: `left: ${(wi * w - w * 0.34).toFixed(3)}%; width: ${(w * 0.68).toFixed(3)}%` }, marks(c)));
      else wi++;
    }
    return out;
  }

  function blockRow(/** @type {any} */ b) {
    return el('div', { class: 'block', 'data-index': b.index }, [
      el('button', { class: 'launch' }),
      el('span', { class: 'idx', text: String(b.index) }),
      el('span', { class: 'sw', 'aria-hidden': 'true', style: `background: ${b.color}` }),
      el('span', { class: 'name' }, [
        el('button', { class: 'jump', 'data-reveal': JSON.stringify(b.path), 'data-label': b.label, title: `Go to song.sequence[${b.index}] (${b.label}) in the doc` }, [el('span', { class: 'jt', text: b.label })]),
        el('span', { class: 'tags' }),
      ]),
      el('span', { class: 'count' }, [el('span', { class: 'n' }), ' loops']),
      el('button', { class: 'loopbtn', 'data-cmd': 'loopBlock', 'data-arg': b.index }, [icon('loop', 11, 2.1)]),
    ]);
  }

  function revRow(/** @type {any} */ r, /** @type {number} */ i) {
    const cur = r.current;
    return el('div', { class: `rev${cur ? ' current' : i % 2 === 0 ? ' alt' : ''}`, 'data-rev': r.rev }, [
      el('span', { class: 'id', text: `r${r.rev}` }),
      el('span', { class: 'when', text: r.when }),
      el('span', { class: 'src' }, [icon(r.source === 'revert' ? 'revert' : 'wave', 10, 2.6), r.source]),
      cur && el('span', { class: `curtag ${cur}` }, [el('span', { class: 'dot', 'aria-hidden': 'true' }), cur === 'playing' ? 'Playing' : 'Loaded']),
      el('span', { class: 'spacer' }),
      el('button', { class: 'rbtn', 'data-cmd': 'diff', 'data-arg': r.rev, 'aria-label': `Diff r${r.rev} against the doc`, title: 'Diff against the doc' }, [icon('diff', 11, 2.1)]),
      r.canRevert
        ? el('button', { class: 'rbtn', 'data-cmd': 'revert', 'data-arg': r.rev, 'aria-label': `Revert to r${r.rev}`, title: 'Revert: put this revision in the editor (undo with ⌘Z)' }, [icon('revert', 11, 2.1)])
        : el('span', { class: 'rbtn blank', 'aria-hidden': 'true' }),
    ]);
  }

  // ── Render ───────────────────────────────────────────────────────────────────────────────────
  function render(/** @type {any} */ m) {
    const h = m.header;
    setText($('piece'), h.piece);
    $('piece').title = h.note ?? '';
    setText($('rev'), h.rev);
    $('revChip').hidden = !h.rev;
    $('unsaved').hidden = !m.unsaved;
    $('pill').className = `pill ${h.pill.kind}`;
    setText($('pillText'), h.pill.text);
    setText($('device'), h.device);
    $('deviceLine').hidden = !h.device;
    $('message').hidden = !h.message;
    setText($('message'), h.message ?? '');
    $('previewBtn').hidden = $('closeBtn').hidden = !m.live;
    $('idle').hidden = m.live;
    $('live').hidden = !m.live;
    if (!m.live) return;

    // Transport
    const t = m.transport;
    press($('play'), t.playing);
    press($('hold'), t.hold);
    press($('drift'), !t.hold);
    press($('orrery'), t.orrery);
    $('orrery').dataset.arg = String(!t.orrery);
    press($('limiter'), t.limiter);
    $('limiter').dataset.arg = String(!t.limiter);
    $('gr').hidden = t.grDb === null;
    setText($('gr'), t.grDb === null ? '' : `GR ${fmtDb(t.grDb)}`);
    $('underruns').hidden = !t.underruns;
    setText($('underruns'), t.underruns ? `${t.underruns} underrun${t.underruns === 1 ? '' : 's'}` : '');
    const r = m.refused;
    $('refused').hidden = !r;
    if (r) {
      keyed($('refused'), 'refused', JSON.stringify(r) + h.rev, () => [
        el('b', { text: `Edit refused (${r.count} error${r.count === 1 ? '' : 's'})` }),
        el('span', { text: '— still playing the last good revision.' }),
        r.path && el('span', { class: 'path', text: r.path }),
        el('span', { text: r.message }),
      ]);
    }

    // Loop readout
    setText($('loopNum'), String(m.loop.counter));
    setText($('posNow'), `${m.loop.posS.toFixed(1)} s`);
    setText($('posOf'), ` / ${m.loop.loopS.toFixed(1)} s`);
    const steps = Math.max(1, m.loop.steps || 16);
    keyed($('timeline'), 'ticks', String(steps), () => [
      el('div', { class: 'played' }),
      ...[...Array(steps - 1)].map((_, k) => el('div', { class: `tick${(k + 1) % 4 === 0 ? ' beat' : ''}`, style: `left: ${(((k + 1) / steps) * 100).toFixed(3)}%` })),
      el('div', { class: 'head' }),
    ]);
    const progress = `${(m.loop.progress * 100).toFixed(2)}%`;
    /** @type {HTMLElement} */ ($('timeline').querySelector('.played')).style.width = progress;
    /** @type {HTMLElement} */ ($('timeline').querySelector('.head')).style.left = progress;

    // Harmony
    const hm = m.harmony;
    setText($('sum-h'), hm.summary);
    $('scaleRoot').hidden = !hm.root;
    setText($('scaleRoot'), hm.root ?? '');
    $('scaleMode').hidden = !hm.mode;
    setText($('scaleMode'), hm.mode ?? '');
    setText($('chord'), hm.chord ?? (t.orrery ? '…' : 'Orrery off'));
    $('chord').classList.toggle('none', !hm.chord);
    setText($('tones'), hm.tones.join(' · '));
    $('tension').classList.toggle('off', hm.tensionLit === null);
    $('tensionSegs').querySelectorAll('.seg-t').forEach((seg, k) => seg.classList.toggle('lit', hm.tensionLit !== null && k < hm.tensionLit));
    $('tensionSegs').setAttribute('aria-label', hm.tension === null ? 'Tension: none (the Orrery is off)' : `Tension ${Math.round(hm.tension * 100)} %`);
    $('keyboard').setAttribute('aria-label', hm.aria);
    const kb = hm.keyboard;
    keyed($('keyboard'), 'kb', kb ? JSON.stringify(kb.cells.map((/** @type {any} */ c) => [c.midi, c.inScale, c.chord, c.pad, c.bass, c.name])) : '', () => (kb ? keyboard(kb) : []));

    // Instruments
    const ins = m.instruments;
    if (keyed($('strips'), 'strips', JSON.stringify(ins.strips.map((/** @type {any} */ s) => [s.id, s.color])), () => ins.strips.map(strip))) {
      // New strips start without their buttons: draw them again.
      for (const k of Object.keys(shapes).filter((k) => k.startsWith('acts:'))) delete shapes[k];
      // Every strip (the master's too) gets the same share; too narrow, and the mixer scrolls.
      const n = Math.max(1, ins.strips.length);
      $('strips').style.flex = `${n} 0 ${n * 46 + (n - 1) * 6}px`;
    }
    for (const s of ins.strips) {
      const row = /** @type {HTMLElement} */ ($('strips').querySelector(`[data-id="${CSS.escape(s.id)}"]`));
      if (!row) continue;
      row.classList.toggle('silenced', s.silenced);
      meter(row, s.id, s.lDb, s.rDb, s.silenced || !t.playing);
      const mute = /** @type {HTMLElement} */ (row.querySelector('.mute'));
      if (mute.getAttribute('aria-pressed') !== String(s.muted)) {
        press(mute, s.muted);
        mute.replaceChildren(icon(s.muted ? 'speakerX' : 'speaker', 11, 2.2));
      }
      mute.setAttribute('aria-label', `${s.muted ? 'Unmute' : 'Mute'} ${s.id}`);
      mute.title = `${s.muted ? 'Unmute' : 'Mute'} ${s.id}`;
      const solo = /** @type {HTMLElement} */ (row.querySelector('.solo'));
      press(solo, s.soloed);
      solo.setAttribute('aria-label', `${s.soloed ? 'Unsolo' : 'Solo'} ${s.id}`);
      solo.title = `${s.soloed ? 'Unsolo' : 'Solo'} ${s.id}`;
      // Companions' buttons, all drawn alike.
      const acts = /** @type {HTMLElement} */ (row.querySelector('.acts'));
      keyed(acts, `acts:${s.id}`, JSON.stringify(s.actions.map((/** @type {any} */ a) => [a.id, a.icon])), () =>
        s.actions.map((/** @type {any} */ a) => el('button', { class: 'sact', 'data-cmd': 'stripAction', 'data-arg': s.id, 'data-action': a.id }, [pathIcon(a.icon, 11, 2)])),
      );
      for (const a of s.actions) {
        const b = /** @type {HTMLElement | null} */ (acts.querySelector(`[data-action="${CSS.escape(a.id)}"]`));
        if (!b) continue;
        press(b, a.on || a.blocked);
        b.classList.toggle('blocked', a.blocked);
        b.setAttribute('aria-label', a.title);
        b.title = a.title;
      }
    }
    const master = meter($('master'), '\u0000master', ins.master.lDb, ins.master.rDb, !t.playing);
    setText($('loopPeak'), fmtDb(ins.master.peakDb));
    $('clearSolo').hidden = !ins.soloed;
    setText($('sum-i'), [`master ${fmtDb(master)} dB`, ins.muted && `${ins.muted} muted`, ins.soloed && `${ins.soloed} soloed`].filter(Boolean).join(' · '));

    // Song
    const song = m.song;
    $('sec-s').hidden = !song;
    if (song) {
      setText($('sum-s'), song.summary);
      const back = /** @type {HTMLButtonElement} */ ($('back'));
      back.disabled = !song.back.enabled;
      back.title = song.back.title;
      $('structure').setAttribute('aria-label', song.aria);
      keyed($('structure'), 'structure', JSON.stringify(song.segments.map((/** @type {any} */ g) => [g.label, g.weight, g.color])), () =>
        song.segments.map((/** @type {any} */ g) =>
          el('div', { class: `sgm${g.tail ? ' tail' : ''}`, title: g.tail ? 'The release tail' : g.label, style: `flex: ${g.weight} 1 0px${g.color ? `; background: ${g.color}` : ''}` }, [
            el('span', { class: 'done', 'aria-hidden': 'true' }),
            el('span', { class: 'head', 'aria-hidden': 'true' }),
            el('span', { class: 'name', text: g.tail ? '' : g.label }),
          ]),
        ),
      );
      $('structure').querySelectorAll('.sgm').forEach((node, k) => {
        const g = song.segments[k];
        const seg = /** @type {HTMLElement} */ (node);
        seg.classList.toggle('current', g.current);
        const done = /** @type {HTMLElement} */ (seg.querySelector('.done'));
        const head = /** @type {HTMLElement} */ (seg.querySelector('.head'));
        done.hidden = head.hidden = !g.current;
        done.style.width = head.style.left = `${(g.frac * 100).toFixed(2)}%`;
      });
      keyed($('blocks'), 'blocks', JSON.stringify(song.rows.map((/** @type {any} */ b) => [b.label, b.loops, b.color])), () => song.rows.map(blockRow));
      $('blocks').querySelectorAll('.block').forEach((node, k) => {
        const b = song.rows[k];
        const row = /** @type {HTMLElement} */ (node);
        row.classList.toggle('playing', b.playing);
        const launch = /** @type {HTMLElement} */ (row.querySelector('.launch'));
        if (launch.dataset.state !== String(b.playing)) {
          launch.dataset.state = String(b.playing);
          launch.classList.toggle('on', b.playing);
          launch.dataset.cmd = b.playing ? 'songStop' : 'playFrom';
          launch.dataset.arg = String(b.index);
          launch.replaceChildren(icon(b.playing ? 'stop' : 'play', b.playing ? 8 : 10));
        }
        const what = b.playing ? `Stop (${b.label} is playing) — back to the loop` : `Play the song from ${b.label}`;
        launch.setAttribute('aria-label', what);
        launch.title = what;
        keyed(/** @type {HTMLElement} */ (row.querySelector('.tags')), `tags${k}`, `${b.playing}${b.from}${b.orrery}`, () => [
          b.playing && el('span', { class: 'tag playing', text: 'Playing' }),
          b.from && el('span', { class: 'tag from', title: 'The song was started from this block', text: 'Play from' }),
          b.orrery === false && el('span', { class: 'tag orrery', title: 'This block’s control turns the Orrery off: its voices play their authored material', text: 'Orrery off' }),
          b.orrery === true && el('span', { class: 'tag orrery', title: 'This block’s control turns the Orrery on', text: 'Orrery on' }),
        ].filter(Boolean));
        setText(/** @type {HTMLElement} */ (row.querySelector('.count .n')), b.count);
        const loop = /** @type {HTMLElement} */ (row.querySelector('.loopbtn'));
        press(loop, b.looping);
        const lw = b.looping ? `Stop looping ${b.label} — back to the loop` : `Loop ${b.label} (edits re-materialize it)`;
        loop.setAttribute('aria-label', lw);
        loop.title = lw;
      });
    }

    // Revisions
    const rv = m.revisions;
    setText($('sum-r'), rv.summary);
    $('allRevs').hidden = !rv.more;
    keyed($('revs'), 'revs', JSON.stringify(rv.rows.map((/** @type {any} */ x) => [x.rev, x.current, x.canRevert, x.source])), () =>
      rv.rows.length ? rv.rows.map(revRow) : [el('div', { class: 'empty', text: 'No revisions yet.' })],
    );
    $('revs').querySelectorAll('.rev .when').forEach((w, k) => setText(/** @type {HTMLElement} */ (w), rv.rows[k]?.when ?? ''));

    // Activity
    const a = m.activity;
    setText($('sum-a'), a.summary);
    setText($('warnings'), a.warnings.length ? `${a.warnings.length} warning${a.warnings.length === 1 ? '' : 's'}: ${a.warnings[0]}` : '');
    $('openPreview').hidden = !a.preview;
    $('revealStems').hidden = !a.stems;
    // Companions' commands.
    keyed($('menu'), 'menu', JSON.stringify(m.menu), () => m.menu.map((/** @type {any} */ i) => el('button', { class: 'link', 'data-cmd': 'menu', 'data-arg': i.command, text: i.title })));
    const pb = /** @type {HTMLButtonElement} */ ($('previewBtn'));
    pb.disabled = a.previewing;
    pb.title = a.previewing ? 'A preview is rendering…' : 'Render a preview (mix or stems)…';
    keyed($('activityItems'), 'act', JSON.stringify(a.items.map((/** @type {any} */ x) => [x.kind, x.text])), () =>
      a.items.map((/** @type {any} */ x) => el('div', { class: `act ${x.kind}` }, [el('span', { class: 'when', text: x.when }), x.text])),
    );
  }

  // ── Events ───────────────────────────────────────────────────────────────────────────────────
  document.addEventListener('click', (ev) => {
    const t = /** @type {HTMLElement} */ (ev.target);
    const toggle = /** @type {HTMLElement | null} */ (t.closest('[data-toggle]'));
    if (toggle) {
      const k = /** @type {string} */ (toggle.dataset.toggle);
      open = { ...open, [k]: !open[k] };
      applySections();
      vscode.setState({ ...(vscode.getState() ?? {}), open });
      vscode.postMessage({ type: 'sections', open });
      return;
    }
    const jump = /** @type {HTMLElement | null} */ (t.closest('[data-reveal]'));
    if (jump) {
      vscode.postMessage({ type: 'reveal', path: JSON.parse(/** @type {string} */ (jump.dataset.reveal)), label: jump.dataset.label });
      return;
    }
    const b = /** @type {HTMLButtonElement | null} */ (t.closest('[data-cmd]'));
    if (!b || b.disabled) return;
    const raw = b.dataset.arg;
    const arg = raw === 'true' ? true : raw === 'false' ? false : raw;
    vscode.postMessage({ type: 'cmd', name: b.dataset.cmd, arg, action: b.dataset.action });
  });

  window.addEventListener('message', (ev) => {
    const msg = ev.data;
    if (msg?.type === 'model') render(msg.model);
    else if (msg?.type === 'init') {
      if (msg.open) open = { ...open, ...msg.open };
      document.body.classList.toggle('reduce-motion', !!msg.reduceMotion);
      applySections();
    }
  });

  build();
  vscode.postMessage({ type: 'ready' });
})();
