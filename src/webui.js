'use strict';
const byId = (id) => document.getElementById(id);
let configuration = null;
let observation = null;
let busy = false;
const controls = [
  ['darkness', 'Druckintensität', '0–30', 'number'],
  ['print_speed', 'Druckgeschwindigkeit', 'Zoll/s', 'number'],
  ['x_offset', 'X-Versatz', 'dots · ^LS', 'number'],
  ['y_offset', 'Y-Versatz', 'dots · ^LT', 'number'],
  ['print_width', 'Druckbreite', 'dots', 'number'],
  ['label_length', 'Etikettenlänge', 'dots · nur Endlos', 'number'],
  ['print_mode', 'Ausgabemodus', '', [['tear_off','Abreißen'],['peel_off','Peel-off / Spenden'],['cutter','Schneiden']]],
  ['print_method', 'Druckverfahren', '', [['direct_thermal','Thermodirekt'],['thermal_transfer','Thermotransfer']]],
  ['tracking', 'Medienerkennung', '', [['gap','Lücke / Gap'],['black_mark','Schwarzmarke'],['continuous','Endlos']]],
];
for (const [key, name, unit, type] of controls) {
  const label = document.createElement('label');
  label.append(document.createTextNode(name));
  if (unit) { const span = document.createElement('span'); span.textContent = unit; label.append(span); }
  const input = document.createElement(Array.isArray(type) ? 'select' : 'input');
  input.id = `device-${key}`;
  if (Array.isArray(type)) {
    input.append(new Option('Nicht ausgelesen', ''));
    for (const [value, title] of type) input.append(new Option(title, value));
  } else { input.type = 'number'; input.step = key === 'darkness' ? '0.1' : '1'; }
  input.disabled = true; label.append(input); byId('device-fields').append(label);
}
function notice(text, tone = 'info') {
  const element = byId('notice'); element.textContent = text; element.dataset.tone = tone; element.hidden = false;
}
function endpoint(suffix) { return `/v1/printers/${encodeURIComponent(byId('printer').value)}${suffix}`; }
async function api(path, {method = 'GET', body} = {}) {
  const token = byId('token').value;
  const headers = {Accept:'application/json'};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(path, {method, headers, body:body === undefined ? undefined : JSON.stringify(body)});
  const result = await response.json().catch(() => null);
  if (!response.ok || result?.error) throw new Error(result?.error?.message || `Anfrage fehlgeschlagen (${response.status})`);
  return result.data;
}
async function run(action) {
  if (busy) return;
  busy = true;
  const disabled = [...document.querySelectorAll('button, select, input')].map(element => [element, element.disabled]);
  disabled.forEach(([element]) => { element.disabled = true; });
  try { await action(); } catch (error) { notice(error.message, 'error'); }
  finally { disabled.forEach(([element, wasDisabled]) => { element.disabled = wasDisabled; }); busy = false; renderAvailability(); }
}
function renderAvailability() {
  const hasPrinter = Boolean(byId('printer').value);
  for (const id of ['read-device','load-roll','save-media','reload']) byId(id).disabled = !hasPrinter;
  byId('unload-roll').disabled = !configuration?.media?.state;
  byId('save-media').disabled = !configuration?.media?.state;
  byId('save-device').disabled = !observation;
  byId('copy-width').disabled = !observation || !(observation.resolution_dpi || configuration?.device?.profile?.resolution_dpi);
  const profile = configuration?.device?.profile || {};
  for (const [key] of controls) {
    const input = byId(`device-${key}`);
    input.disabled = observation?.settings?.[key] == null;
    if (key === 'label_length') input.disabled ||= byId('device-tracking').value !== 'continuous';
    if (key === 'darkness') { input.min = '0'; input.max = '30'; }
    if (key === 'print_speed') { input.min = '1'; input.max = profile.max_speed_ips; }
    if (['x_offset','y_offset'].includes(key)) { input.min = -profile.max_offset_dots; input.max = profile.max_offset_dots; }
    if (key === 'print_width') { input.min = '1'; input.max = profile.max_width_dots; }
    if (key === 'label_length') { input.min = '1'; input.max = profile.max_length_dots; }
  }
  byId('device-print_mode').querySelector('[value=peel_off]').disabled = !profile.peel_off;
  byId('device-print_mode').querySelector('[value=cutter]').disabled = !profile.cutter;
  byId('device-print_method').querySelector('[value=thermal_transfer]').disabled = !profile.thermal_transfer;
}
function renderDevice() {
  for (const [key] of controls) byId(`device-${key}`).value = observation?.settings?.[key] ?? '';
  byId('device-status').textContent = observation ? `Zuletzt ausgelesen: ${new Date(observation.observed_at).toLocaleString('de-DE')}. Vor dem Speichern wird erneut geprüft.` : 'Noch keine Gerätewerte ausgelesen. Unbekannte Werte werden nicht geraten.';
  byId('diagnostics').textContent = JSON.stringify(configuration?.device?.last_save || {}, null, 2) + '\n\n' + (observation?.raw || '');
  byId('confirm-save').checked = false;
  renderAvailability();
}
function renderMedia() {
  const state = configuration?.media?.state;
  if (!state) {
    byId('media-form').reset();
    byId('media-title').textContent = 'Keine Rolle erfasst';
    byId('media-summary').textContent = 'Neue Rolle einlegen, um sie zu verwalten';
    byId('media-count').textContent = '';
  } else {
    const media = state.media;
    byId('media-name').value = media.display_name;
    byId('media-width').value = media.width_mm; byId('media-height').value = media.height_mm;
    byId('media-color-name').value = media.color.name; byId('media-color').value = media.color.hex || '#ffffff';
    byId('media-tracking').value = media.tracking; byId('media-technology').value = media.print_technology;
    byId('media-warning').value = media.low_warning_threshold; byId('media-quantity').value = state.initial_labels;
    byId('media-title').textContent = media.display_name;
    byId('media-summary').textContent = `${media.width_mm} × ${media.height_mm} mm · ${media.color.name}`;
    byId('media-count').textContent = `${state.remaining_labels} verbleibend · ${state.consumed_labels_total} verbraucht (geschätzt)`;
  }
  byId('swatch').setAttribute('fill', byId('media-color').value);
}
async function reload() {
  if (!byId('printer').value) return;
  configuration = await api(endpoint('/configuration'));
  observation = configuration.device.observation;
  renderMedia(); renderDevice();
}
function mediaDefinition() {
  const previous = configuration?.media?.state?.media || {shape:'rectangle', preferred_settings:{}, custom:{}};
  return {...previous, display_name:byId('media-name').value.trim(), width_mm:Number(byId('media-width').value),
    height_mm:Number(byId('media-height').value), color:{...previous.color, name:byId('media-color-name').value.trim(), hex:byId('media-color').value},
    tracking:byId('media-tracking').value, print_technology:byId('media-technology').value,
    labels_available_at_load:Number(byId('media-quantity').value), low_warning_threshold:Number(byId('media-warning').value)};
}
byId('printer').addEventListener('change', () => { configuration = null; observation = null; renderMedia(); renderDevice(); void run(reload); });
byId('reload').addEventListener('click', () => void run(reload));
byId('media-color').addEventListener('input', () => byId('swatch').setAttribute('fill', byId('media-color').value));
byId('device-tracking').addEventListener('change', renderAvailability);
byId('read-device').addEventListener('click', () => void run(async () => {
  observation = null; renderDevice();
  observation = await api(endpoint('/configuration/read'), {method:'POST'});
  renderDevice(); notice('Aktuelle Gerätewerte ausgelesen.');
}));
byId('media-form').addEventListener('submit', event => {
  event.preventDefault();
  const body = {revision:configuration.media.revision, media:mediaDefinition()};
  void run(async () => { await api(endpoint('/media'), {method:'PATCH', body}); await reload(); notice('Rollendaten bei ZebraTamer gespeichert. Der Drucker wurde nicht verändert.', 'success'); });
});
byId('load-roll').addEventListener('click', () => {
  if (!byId('media-form').reportValidity()) return;
  if (!window.confirm('Neue Rolle einlegen? Der bisherige Rollenstand wird archiviert und der Zähler für die neue Rolle beginnt mit der eingegebenen Anzahl.')) return;
  const body = mediaDefinition();
  void run(async () => { await api(endpoint('/media'), {method:'PUT', body}); await reload(); notice('Neue Rolle bei ZebraTamer erfasst. Gerätevorgaben bei Bedarf separat speichern.', 'success'); });
});
byId('unload-roll').addEventListener('click', () => {
  if (!window.confirm('Eingelegte Rolle entfernen? Ihr letzter Stand wird archiviert.')) return;
  void run(async () => { await api(endpoint('/media/unload'), {method:'POST'}); await reload(); notice('Rolle entfernt.', 'success'); });
});
byId('copy-width').addEventListener('click', () => {
  const dpi = observation?.resolution_dpi || configuration?.device?.profile?.resolution_dpi;
  if (!dpi) return;
  if (!byId('device-print_width').disabled) byId('device-print_width').value = Math.round(Number(byId('media-width').value) * dpi / 25.4);
  if (!byId('device-label_length').disabled) byId('device-label_length').value = Math.round(Number(byId('media-height').value) * dpi / 25.4);
  notice('Maße ins Geräteformular übernommen. Noch nichts an den Drucker gesendet. Bei Gap/Mark-Medien bestimmt die Kalibrierung die Länge.');
});
byId('device-form').addEventListener('submit', event => {
  event.preventDefault();
  const settings = {};
  for (const [key,,, type] of controls) {
    const input = byId(`device-${key}`);
    if (input.disabled || input.value === '') continue;
    const value = Array.isArray(type) ? input.value : Number(input.value);
    if (value !== observation.settings[key]) settings[key] = value;
  }
  if (!Object.keys(settings).length) { notice('Keine Gerätewerte geändert. Es wird kein Speicherbefehl gesendet.'); return; }
  const body = {revision:observation.revision, settings, confirm_save_all:byId('confirm-save').checked};
  void run(async () => {
    const result = await api(endpoint('/configuration'), {method:'POST', body});
    await reload();
    if (result.state === 'save_sent_active_verified') notice('Speicherbefehl gesendet; aktive Werte stimmen überein. Die Persistenz nach Aus-/Einschalten wurde nicht geprüft.', 'success');
    else notice(result.error || 'Ergebnis unklar. Vor einem erneuten Versuch Gerätewerte auslesen.', 'error');
  });
});
void run(async () => {
  const printers = await api('/v1/printers');
  byId('printer').replaceChildren();
  for (const printer of printers) byId('printer').append(new Option(printer.display_name || printer.id, printer.id));
  const requested = new URLSearchParams(location.search).get('printer');
  if (printers.some(printer => printer.id === requested)) byId('printer').value = requested;
  if (!printers.length) { byId('printer').append(new Option('Keine Drucker konfiguriert', '')); notice('In config.toml ist noch kein Drucker eingetragen.'); }
  else await reload();
});
