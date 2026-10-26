const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const content = $('content');
const caption = $('caption');
const notice = $('notice');

const STATUS = {
  transcribing: 'Leyendo la imagen…',
  thinking: 'Analizando con más detalle…',
  reading: 'Leyendo en voz alta',
  paused: 'En pausa',
  stopped: 'Detenido',
  done: 'Terminado',
  error: 'Error',
};

/** id de segmento → { el, kind, words, display, block, spoken } */
let segs = new Map();
let order = [];
let paragraph = null;
let current = null;
/** Fragmento de fórmula resaltado (dentro de `current`). */
let currentPart = null;
let status = 'done';

listen('reader:event', ({ payload }) => handlers[payload.type]?.(payload));

const handlers = {
  reset() {
    segs = new Map();
    order = [];
    paragraph = null;
    current = null;
    currentPart = null;
    content.innerHTML = '<div class="skeleton"><div></div><div></div><div></div></div>';
    caption.hidden = true;
    notice.hidden = true;
    $('badge').hidden = true;
    setStatus('transcribing', '');
  },
  segment({ seg }) {
    render(seg);
  },
  mathSpoken({ seg, spoken, pieces, annotated, kinds }) {
    const s = segs.get(seg);
    if (!s) return;
    s.spoken = spoken;
    s.pieces = pieces || [];
    s.kinds = kinds || [];
    s.el.title = spoken;
    if (annotated) markParts(s, annotated);
    if (current === s.el) showCaption(s);
  },
  highlight({ seg, word }) {
    highlight(seg, word);
  },
  status({ status, detail }) {
    setStatus(status, detail);
  },
  lang({ lang, voice }) {
    const badge = $('badge');
    badge.textContent = `${lang === 'en' ? 'Inglés' : 'Español'} · ${voice}`;
    badge.hidden = false;
  },
  notice({ msg }) {
    notice.textContent = msg;
    notice.hidden = false;
  },
};

function setStatus(s, detail) {
  status = s;
  $('status').textContent = STATUS[s] || s;
  $('detail').textContent = detail ? `· ${detail}` : '';
  $('dot').className = `dot ${s}`;
  const playing = s === 'reading' || s === 'transcribing' || s === 'thinking';
  // `hidden` no aplica a elementos SVG: se usa display.
  $('icon-pause').style.display = playing ? '' : 'none';
  $('icon-play').style.display = playing ? 'none' : '';
  if (s === 'error' && detail) handlers.notice({ msg: detail });
  if (s === 'done' || s === 'stopped') {
    caption.hidden = true;
    current?.classList.remove('hl');
    currentPart?.classList.remove('hlp');
    current = null;
    currentPart = null;
  }
}

function block(tag, cls) {
  content.querySelector('.skeleton, .placeholder')?.remove();
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  content.appendChild(el);
  return el;
}

function wordsInto(el, seg) {
  seg.words.forEach((w, i) => {
    if (i) el.appendChild(document.createTextNode(' '));
    const span = document.createElement('span');
    span.className = 'w';
    span.textContent = w;
    span.dataset.seg = seg.id;
    span.dataset.word = i;
    el.appendChild(span);
  });
}

function mathInto(el, tex, display) {
  try {
    katex.render(tex, el, { displayMode: display, throwOnError: false });
  } catch {
    el.textContent = tex;
  }
  el.classList.add('m');
}

function render(seg) {
  let el;
  if (seg.kind === 'heading') {
    el = block('h2');
    wordsInto(el, seg);
    paragraph = null;
  } else if (seg.kind === 'math' && seg.block) {
    el = block('div', 'math-block');
    mathInto(el, seg.display, true);
    paragraph = null;
  } else {
    if (!paragraph || seg.para) paragraph = block('p');
    else paragraph.appendChild(document.createTextNode(' '));
    el = document.createElement('span');
    if (seg.kind === 'math') mathInto(el, seg.display, false);
    else wordsInto(el, seg);
    paragraph.appendChild(el);
  }
  el.dataset.seg = seg.id;
  segs.set(seg.id, {
    el,
    kind: seg.kind,
    words: [...el.querySelectorAll('.w')],
    display: seg.display,
    block: seg.block,
    para: seg.para,
    spoken: '',
    pieces: [],
    /** por frase: 'part' (tiene fragmento), 'link' («entre», «por») o 'lost' (no se pudo marcar) */
    kinds: [],
    /** frase → elemento `[data-part]` de la fórmula marcada */
    parts: new Map(),
    piece: null,
  });
  order.push(seg.id);
}

/**
 * Vuelve a dibujar la fórmula con cada fragmento envuelto en `\htmlData{part=i}`.
 * Si KaTeX rechaza una marca se quita sólo esa y se reintenta, para no perder las demás;
 * si aun así falla se queda el dibujo original (y se resalta la fórmula completa).
 */
function markParts(s, annotated) {
  const holder = document.createElement('span');
  const log = (msg) => invoke('ui_log', { msg: `lector: ${msg}` }).catch(() => {});
  let dropped = 0;
  for (;;) {
    try {
      katex.render(annotated, holder, {
        displayMode: s.block,
        throwOnError: true,
        trust: (ctx) => ctx.command === '\\htmlData',
        strict: 'ignore',
      });
      break;
    } catch (e) {
      const next = dropped < 30 ? dropMark(annotated, e.position) : null;
      if (next == null) return log(`no se pudo marcar la fórmula (${e.message})`);
      annotated = next;
      dropped++;
    }
  }
  if (dropped) log(`se quitaron ${dropped} marcas que KaTeX rechazó`);
  if (currentPart && s.el.contains(currentPart)) currentPart = null;
  s.el.replaceChildren(...holder.childNodes);
  s.parts = new Map([...s.el.querySelectorAll('[data-part]')].map((el) => [Number(el.dataset.part), el]));
}

/**
 * Quita la marca `\htmlData{part=N}{…}` donde KaTeX encontró el error (la última que empieza
 * antes de `pos`; sin posición, la última) y deja su contenido. `null` si no queda ninguna.
 */
function dropMark(tex, pos) {
  let mark = null;
  for (const m of tex.matchAll(/\\htmlData\{part=\d+\}\{/g)) {
    if (pos == null || m.index <= pos) mark = m;
  }
  if (!mark) return null;
  const open = mark.index + mark[0].length;
  let depth = 1;
  let i = open;
  for (; i < tex.length && depth; i++) {
    if (tex[i] === '\\') i++;
    else if (tex[i] === '{') depth++;
    else if (tex[i] === '}') depth--;
  }
  if (depth) return null;
  return tex.slice(0, mark.index) + tex.slice(open, i - 1) + tex.slice(i);
}

function highlight(segId, word) {
  const s = segs.get(segId);
  if (!s) return;
  if (s.kind === 'math') return highlightMath(s, word);
  const target = word == null ? s.el : s.words[word] || s.el;
  if (target === current) return;
  current?.classList.remove('hl');
  currentPart?.classList.remove('hlp');
  currentPart = null;
  target.classList.add('hl');
  current = target;
  caption.hidden = true;
  ensureVisible(target);
}

/** La fórmula completa lleva contorno y el fragmento de la frase que suena, fondo amarillo. */
function highlightMath(s, piece) {
  if (current !== s.el) {
    current?.classList.remove('hl');
    currentPart?.classList.remove('hlp');
    currentPart = null;
    s.el.classList.add('hl');
    current = s.el;
    ensureVisible(s.el);
  }
  if (piece != null) {
    s.piece = piece;
    const part = s.parts.get(piece);
    if (part && part !== currentPart) {
      currentPart?.classList.remove('hlp');
      part.classList.add('hlp');
      currentPart = part;
    } else if (!part && s.kinds[piece] !== 'link') {
      // Sin fragmento marcado: mejor sólo el contorno que una parte equivocada.
      // Las palabras de enlace («entre», «por») sí dejan el fragmento anterior.
      currentPart?.classList.remove('hlp');
      currentPart = null;
    }
  }
  showCaption(s);
}

/** Subtítulo con la lectura de la fórmula; la frase que suena va marcada. */
function showCaption(s) {
  const pieces = s.pieces.length ? s.pieces : [s.spoken || '…'];
  caption.replaceChildren();
  pieces.forEach((text, i) => {
    if (i) caption.append(' ');
    if (pieces.length > 1 && i === s.piece) {
      const mark = document.createElement('mark');
      mark.textContent = text;
      caption.append(mark);
    } else {
      caption.append(text);
    }
  });
  caption.hidden = false;
}

function ensureVisible(el) {
  const r = el.getBoundingClientRect();
  const c = content.getBoundingClientRect();
  if (r.top < c.top + 8 || r.bottom > c.bottom - 8) {
    el.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }
}

/** Texto plano con las fórmulas en LaTeX, para copiar. */
function plainText() {
  let out = '';
  for (const id of order) {
    const s = segs.get(id);
    if (s.kind === 'heading') out += `\n\n${s.display}\n\n`;
    else if (s.kind === 'math' && s.block) out += `\n\n$$${s.display}$$\n\n`;
    else {
      if (s.para) out += '\n\n';
      else if (out && !out.endsWith('\n')) out += ' ';
      out += s.kind === 'math' ? `$${s.display}$` : s.display;
    }
  }
  return out.replace(/\n{3,}/g, '\n\n').trim();
}

// ----------------------------------------------------------------- controles

$('play').onclick = () => invoke('player_toggle');
$('stop').onclick = () => invoke('player_stop');
$('prev').onclick = () => invoke('player_seek', { delta: -1 });
$('next').onclick = () => invoke('player_seek', { delta: 1 });
$('back2').onclick = () => invoke('player_seek_words', { delta: -2 });
$('fwd2').onclick = () => invoke('player_seek_words', { delta: 2 });
$('repeat').onclick = () => invoke('player_seek', { delta: 0 });
$('rate').onchange = (e) => invoke('set_rate', { rate: parseFloat(e.target.value) });
$('copy').onclick = async () => {
  try {
    await navigator.clipboard.writeText(plainText());
    handlers.notice({ msg: 'Texto copiado al portapapeles.' });
    setTimeout(() => (notice.hidden = true), 1500);
  } catch (e) {
    handlers.notice({ msg: `No se pudo copiar: ${e}` });
  }
};
$('pin').onclick = () => {
  const on = !$('pin').classList.contains('on');
  $('pin').classList.toggle('on', on);
  invoke('reader_pin', { on });
};

// Clic en una palabra (o en una parte de una fórmula): leer desde ahí.
content.addEventListener('click', (e) => {
  const el = e.target.closest('[data-seg]');
  if (!el) return;
  const word = e.target.closest('.w');
  const part = e.target.closest('[data-part]');
  let index = null;
  if (word) index = Number(word.dataset.word);
  else if (part && el.contains(part)) index = Number(part.dataset.part);
  invoke('player_seek_segment', { seg: Number(el.dataset.seg), word: index });
});

window.addEventListener('keydown', (e) => {
  if (e.target.tagName === 'SELECT') return;
  if (e.key === ' ') {
    e.preventDefault();
    invoke('player_toggle');
  } else if (e.key === 'Escape') {
    invoke('reader_close');
  } else if (e.key === 'ArrowLeft') {
    // ← / → saltan 2 palabras; con Ctrl (o ⌘ en Mac), una oración.
    if (e.ctrlKey || e.metaKey) invoke('player_seek', { delta: -1 });
    else invoke('player_seek_words', { delta: -2 });
  } else if (e.key === 'ArrowRight') {
    if (e.ctrlKey || e.metaKey) invoke('player_seek', { delta: 1 });
    else invoke('player_seek_words', { delta: 2 });
  } else if (e.key === 'r' || e.key === 'R') {
    invoke('player_seek', { delta: 0 });
  }
});

invoke('get_config').then((cfg) => {
  const rate = String(cfg.tts.rate);
  const sel = $('rate');
  if (![...sel.options].some((o) => o.value === rate)) sel.add(new Option(`${rate}×`, rate));
  sel.value = rate;
  $('pin').classList.toggle('on', cfg.ui.always_on_top);
  const placeholder = document.querySelector('.placeholder');
  if (placeholder) {
    placeholder.textContent = `Pulsa ${cfg.hotkey} y selecciona una zona de la pantalla para escucharla.`;
  }
});
