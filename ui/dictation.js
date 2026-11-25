import { button, check, el, field, options, uuid } from './dom.js';

const endpoints = {
  open_ai: ['OpenAI', 'https://api.openai.com/v1/audio/transcriptions', 'gpt-4o-mini-transcribe'],
  open_ai_compatible: ['OpenAI-compatible', 'http://127.0.0.1:8000/v1/audio/transcriptions', 'whisper-1'],
  eleven_labs: ['ElevenLabs', 'https://api.elevenlabs.io/v1/speech-to-text', 'scribe_v2'],
  hermes: ['Hermes dashboard', 'http://127.0.0.1:9119/api/audio/transcribe', ''],
};
const modelSize = (model) => Math.round(model.bytes / 1_000_000) + ' MB';

export function dictationSettings(api, heading, section, render, run) {
  return function page() {
    const saved = api.data().dictation ?? {};
    const savedEndpoint = saved.endpoint;
    let customModel = saved.custom_model ?? null;
    let catalog = [];
    let forget = false;
    let downloading = false;
    const mode = options([['off', 'Off'], ['local', 'Local Whisper'], ['endpoint', 'Transcription endpoint']], saved.mode ?? 'off');
    const language = el('input', { value: saved.language ?? '', placeholder: 'Automatic', maxlength: 3, spellcheck: 'false' });
    const status = el('div', { class: 'form-status', role: 'status' });
    const downloadStatus = el('span', { class: 'field-note', role: 'status' });
    const endpointKind = options(Object.entries(endpoints).map(([id, config]) => [id, config[0]]), savedEndpoint?.kind ?? 'open_ai');
    const defaultEndpoint = endpoints[endpointKind.value];
    const url = el('input', { type: 'url', value: savedEndpoint?.url ?? defaultEndpoint[1] });
    const remoteModel = el('input', { value: savedEndpoint?.model ?? defaultEndpoint[2], maxlength: 300, spellcheck: 'false' });
    const keyHeader = el('input', { value: savedEndpoint?.key_header ?? '', placeholder: 'Adapter default', spellcheck: 'false' });
    const key = el('input', { type: 'password', autocomplete: 'off', placeholder: savedEndpoint?.has_secret ? 'Key saved' : '' });
    const endpointId = savedEndpoint?.id ?? uuid();
    const startingModel = saved.local_model ?? 'base-q5_1';
    const model = options([[startingModel, 'Loading Whisper models…']], startingModel);
    const path = el('span', { class: 'field-note' }, customModel ?? 'No file selected');
    const save = el('button', { type: 'submit', class: 'button primary' }, 'Save dictation settings');
    const choose = button('Choose model file', () => run(choose, status, async () => {
      const file = await api.io.open({ title: 'Choose a Whisper GGML model', filters: [{ name: 'Whisper GGML model', extensions: ['bin'] }] });
      if (file) { customModel = file; path.textContent = file; }
      status.textContent = '';
    }), { glyph: 'folder' });
    const modelFile = el('div', { class: 'form-actions' }, choose, path);
    const download = button('Download selected model', () => {
      const selected = catalog.find((item) => item.id === model.value);
      if (!selected || download.disabled) return;
      api.confirm('Download Whisper model',
        'OpenDex will download ' + selected.label + ' (' + modelSize(selected) + ') from Hugging Face and verify it before installation.',
        async () => {
          downloading = true;
          update();
          try {
            await run(download, downloadStatus, async () => {
              await api.invoke('download_dictation_model', { model: selected.id });
              await loadModels();
            });
          } finally {
            downloading = false;
            update();
          }
        }, 'Download');
    }, { glyph: 'download' });
    const downloadRow = el('div', { class: 'form-actions' }, download, downloadStatus);
    const localFields = el('div', { class: 'form-grid wide' },
      field('Local model', model), el('div', { class: 'wide' }, modelFile, downloadRow));
    const remoteModelField = field('Model ID', remoteModel);
    const headerField = field('Key header', keyHeader, 'A custom header receives the saved key as entered.');
    const remoteFields = el('div', { class: 'form-grid wide' },
      field('Endpoint type', endpointKind), field('Transcription URL', url),
      remoteModelField, field('API key or dashboard token', key), headerField,
      savedEndpoint?.has_secret && check('Forget saved endpoint key', false, (value) => { forget = value; }),
      el('p', { class: 'field-note wide' }, 'OpenDex sends the recorded audio to this endpoint when you stop dictation.'));
    const languageField = field('Language code', language, 'Leave blank for automatic detection.');

    function update() {
      const local = mode.value === 'local';
      const remote = mode.value === 'endpoint';
      const hermes = endpointKind.value === 'hermes';
      localFields.hidden = !local;
      remoteFields.hidden = !remote;
      languageField.hidden = !(local || (remote && !hermes));
      url.required = remote;
      url.disabled = !remote;
      url.readOnly = ['open_ai', 'eleven_labs'].includes(endpointKind.value);
      remoteModel.required = remote && !hermes;
      remoteModelField.hidden = hermes;
      headerField.hidden = url.readOnly;
      modelFile.hidden = model.value !== 'custom';
      downloadRow.hidden = model.value === 'custom';
      const selected = catalog.find((item) => item.id === model.value);
      const ready = local && saved.mode === 'local' && saved.local_model === model.value;
      download.disabled = downloading || !ready || !selected || selected.installed;
      if (!downloading && !downloadStatus.classList.contains('error')) {
        downloadStatus.textContent = selected?.installed ? 'Installed'
          : ready ? 'Download requires approval.' : 'Save this local selection to enable its download.';
      }
    }

    async function loadModels() {
      const selected = model.value;
      catalog = await api.invoke('dictation_models');
      model.replaceChildren(...catalog.map((item) =>
        el('option', { value: item.id }, item.label + ' (' + modelSize(item) + ')')),
      el('option', { value: 'custom' }, 'Use an existing GGML file'));
      model.value = selected;
      update();
    }

    mode.addEventListener('change', update);
    model.addEventListener('change', () => {
      downloadStatus.classList.remove('error');
      update();
    });
    endpointKind.addEventListener('change', () => {
      const [, address, modelId] = endpoints[endpointKind.value];
      url.value = address;
      remoteModel.value = modelId;
      key.value = '';
      keyHeader.value = '';
      update();
    });

    const form = el('form', { class: 'form-grid', onsubmit: (event) => {
      event.preventDefault();
      void run(save, status, async () => {
        const remote = mode.value === 'endpoint';
        try {
          await api.invoke('save_dictation_settings', {
            settings: {
              mode: mode.value, local_model: model.value, custom_model: customModel, language: language.value,
              endpoint: remote ? {
                id: endpointId, kind: endpointKind.value, url: url.value, model: remoteModel.value,
                key_header: keyHeader.value,
              } : savedEndpoint ?? null,
            },
            secret: remote ? key.value.trim() || null : null,
            forget: remote && forget,
          });
        } finally { key.value = ''; }
        await api.refresh();
        render();
        api.toast('Dictation settings saved.');
      });
    } },
    field('Transcription source', mode), languageField, localFields, remoteFields,
    el('div', { class: 'form-actions wide' }, save, status));

    update();
    void loadModels().catch((error) => { status.textContent = String(error); });
    return el('div', { class: 'settings-page' },
      heading('Dictation', 'Click the microphone to record, then click it again to transcribe and send.'),
      section('Speech to text', 'Local Whisper processes audio on this computer. Download a model only after you select and approve it, or use a GGML model file you already have.', form));
  };
}

export function dictationRecorder(invoke, changed, transcribed, error) {
  let current;

  function clear(job) {
    clearTimeout(job.timer);
    if (current !== job) return false;
    current = null;
    changed();
    return true;
  }

  async function finish(job) {
    if (current !== job || job.stage !== 'recording') return;
    clearTimeout(job.timer);
    job.stage = 'transcribing';
    changed();
    let text;
    try {
      text = (await invoke('finish_dictation', { id: job.id }))?.trim();
    } catch (reason) {
      if (clear(job)) error(String(reason), true);
      return;
    }
    if (!clear(job)) return;
    if (!text) { error('No speech was recognized.'); return; }
    try {
      await transcribed(job.thread, text);
    } catch (reason) {
      error(String(reason), true);
    }
  }

  async function toggle(thread) {
    if (current) {
      await finish(current);
      return;
    }
    const job = { id: uuid(), thread, stage: 'starting' };
    current = job;
    changed();
    try {
      await invoke('start_dictation', { id: job.id, thread });
      if (current !== job) {
        await invoke('cancel_dictation', { id: job.id });
        return;
      }
      job.stage = 'recording';
      job.timer = setTimeout(() => void finish(job), 119_000);
      changed();
    } catch (reason) {
      if (clear(job)) error(String(reason), true);
    }
  }

  function keepThread(thread) {
    const job = current;
    if (job && job.thread !== thread && job.stage !== 'transcribing' && clear(job)) {
      void invoke('cancel_dictation', { id: job.id }).catch(() => {});
    }
  }

  function cancel() {
    const job = current;
    if (job && job.stage !== 'transcribing' && clear(job)) {
      void invoke('cancel_dictation', { id: job.id }).catch(() => {});
    }
  }

  return { toggle, keepThread, cancel, current: () => current };
}
