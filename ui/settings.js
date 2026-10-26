const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let cfg = null;
/** motor → lista de voces (se piden una sola vez) */
const voiceCache = {};

function engine() {
  return document.querySelector('input[name=engine]:checked')?.value || 'system';
}

function setMsg(el, text, ok) {
  el.textContent = text;
  el.className = (el.className.replace(/\b(ok|err)\b/g, '') + (ok === undefined ? '' : ok ? ' ok' : ' err')).trim();
}

async function voices(eng) {
  if (!voiceCache[eng]) voiceCache[eng] = invoke('list_voices', { engine: eng });
  try {
    return await voiceCache[eng];
  } catch (e) {
    delete voiceCache[eng];
    throw e;
  }
}

function fill(select, list, prefix, selected, withDefault) {
  select.innerHTML = '';
  // Vacío = la voz que el sistema tenga configurada para ese idioma.
  if (withDefault) select.add(new Option('Predeterminada del sistema', ''));
  const matching = list.filter((v) => v.locale.toLowerCase().startsWith(prefix));
  for (const v of matching.length ? matching : list) {
    select.add(new Option(`${v.name} (${v.locale})`, v.id));
  }
  if (selected && ![...select.options].some((o) => o.value === selected)) {
    select.add(new Option(`${selected} (no instalada)`, selected), 0);
  }
  select.value = selected;
}

async function showVoices() {
  const eng = engine();
  const keyEs = `${eng}_es`, keyEn = `${eng}_en`;
  setMsg($('voice_msg'), eng === 'edge' ? 'Cargando voces de Edge…' : '');
  try {
    const list = await voices(eng);
    fill($('voice_es'), list, 'es', cfg.tts[keyEs], eng === 'system');
    fill($('voice_en'), list, 'en', cfg.tts[keyEn], eng === 'system');
    setMsg($('voice_msg'), eng === 'edge' ? 'Las voces de Edge necesitan internet.' : '');
  } catch (e) {
    setMsg($('voice_msg'), `No se pudieron cargar las voces: ${e}`, false);
  }
}

/** Guarda en `cfg` lo que muestra el formulario. */
function collect() {
  const eng = engine();
  cfg.hotkey = $('hotkey').value;
  cfg.llm.model = $('model').value.trim();
  cfg.llm.precise_mode = $('precise').checked;
  cfg.llm.adaptive = $('adaptive').checked;
  cfg.tts.engine = eng;
  // En las voces del sistema el valor vacío es válido (la predeterminada).
  if ($('voice_es').value || eng === 'system') cfg.tts[`${eng}_es`] = $('voice_es').value;
  if ($('voice_en').value || eng === 'system') cfg.tts[`${eng}_en`] = $('voice_en').value;
  cfg.tts.rate = parseFloat($('rate').value);
  cfg.ui.default_language = $('default_language').value;
  cfg.ui.always_on_top = $('always_on_top').checked;
  cfg.ui.math_parts = $('math_parts').checked;
}

async function load() {
  cfg = await invoke('get_config');
  $('hotkey').value = cfg.hotkey;
  $('model').value = cfg.llm.model;
  showKeyStatus(await invoke('api_key_status'));
  $('precise').checked = cfg.llm.precise_mode;
  $('adaptive').checked = cfg.llm.adaptive;
  document.querySelector(`input[name=engine][value=${cfg.tts.engine}]`).checked = true;
  $('rate').value = String(cfg.tts.rate);
  $('default_language').value = cfg.ui.default_language;
  $('always_on_top').checked = cfg.ui.always_on_top;
  $('math_parts').checked = cfg.ui.math_parts;
  await showVoices();
}

// Captura del atajo: clic en el campo y pulsar la combinación.
const hotkey = $('hotkey');
hotkey.addEventListener('focus', () => hotkey.classList.add('capturing'));
hotkey.addEventListener('blur', () => hotkey.classList.remove('capturing'));
hotkey.addEventListener('keydown', (e) => {
  e.preventDefault();
  if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return;
  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');
  let key = e.code;
  if (key.startsWith('Key')) key = key.slice(3);
  else if (key.startsWith('Digit')) key = key.slice(5);
  if (!parts.length) return setMsg($('msg'), 'El atajo necesita Ctrl, Alt, Shift o Win.', false);
  hotkey.value = [...parts, key].join('+');
  setMsg($('msg'), '');
  hotkey.blur();
});

for (const radio of document.querySelectorAll('input[name=engine]')) {
  radio.addEventListener('change', () => {
    collect();
    showVoices();
  });
}

/** La key nunca vuelve a la interfaz: sólo si existe, su origen y sus últimos 4 caracteres. */
function showKeyStatus(st) {
  const from = st.source === 'env' ? 'la variable de entorno ANTHROPIC_API_KEY' : 'el Administrador de credenciales de Windows';
  if (st.present) setMsg($('key_msg'), `Key guardada en ${from} (…${st.hint}).`, true);
  else setMsg($('key_msg'), 'Falta la API key: sin ella se usa el OCR de Windows (sin fórmulas).', false);
}

$('save_key').onclick = async () => {
  const key = $('api_key').value.trim();
  try {
    showKeyStatus(await invoke('set_api_key', { key }));
    $('api_key').value = '';
    if (key) $('test_llm').onclick();
  } catch (e) {
    setMsg($('key_msg'), String(e), false);
  }
};

$('test_llm').onclick = async () => {
  setMsg($('llm_msg'), 'Probando…');
  try {
    setMsg($('llm_msg'), await invoke('llm_status', { model: $('model').value.trim() }), true);
  } catch (e) {
    setMsg($('llm_msg'), String(e), false);
  }
};

async function testVoice(lang) {
  const voice = $(lang === 'es' ? 'voice_es' : 'voice_en').value;
  setMsg($('voice_msg'), 'Generando muestra…');
  try {
    await invoke('test_voice', { engine: engine(), voice, lang });
    setMsg($('voice_msg'), '');
  } catch (e) {
    setMsg($('voice_msg'), String(e), false);
  }
}
$('test_es').onclick = () => testVoice('es');
$('test_en').onclick = () => testVoice('en');

$('save').onclick = async () => {
  collect();
  try {
    await invoke('save_config', { cfg });
    setMsg($('msg'), 'Guardado.', true);
  } catch (e) {
    setMsg($('msg'), String(e), false);
  }
};
$('close').onclick = () => window.close();

load().catch((e) => setMsg($('msg'), String(e), false));
