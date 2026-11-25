import { button, check, el, field, options, uuid } from './dom.js';

const adapters = {
  open_ai: {
    name: 'OpenAI',
    endpoint: 'https://api.openai.com/v1/audio/speech',
    model: 'gpt-4o-mini-tts',
    voice: 'marin',
    fixedEndpoint: true,
    presets: ['alloy', 'ash', 'ballad', 'cedar', 'coral', 'echo', 'fable', 'marin', 'nova', 'onyx', 'sage', 'shimmer', 'verse'],
  },
  eleven_labs: {
    name: 'ElevenLabs',
    endpoint: 'https://api.elevenlabs.io/',
    model: 'eleven_multilingual_v2',
    voice: 'JBFqnCBsd6RMkjVDRZzb',
    fixedEndpoint: true,
  },
  pocket_tts: {
    name: 'Pocket TTS',
    endpoint: 'http://127.0.0.1:8000/tts',
    presets: ['alba', 'azelma', 'cosette', 'eponine', 'fantine', 'javert', 'jean', 'marius'],
  },
  piper: {
    name: 'Piper HTTP',
    endpoint: 'http://127.0.0.1:5000/synthesize',
    endpointNote: 'Use the full Piper JSON synthesis URL. Older servers may use the root URL.',
  },
  omni_voice: {
    name: 'OmniVoice',
    endpoint: 'http://127.0.0.1:8880/v1/audio/speech',
    model: 'omnivoice',
    voice: 'auto',
    endpointNote: 'Use your OmniVoice server’s OpenAI-compatible speech URL.',
    voiceNote: 'Use auto for the server default or enter a voice ID from your server.',
  },
  kokoro: {
    name: 'Kokoro-FastAPI',
    endpoint: 'http://127.0.0.1:8880/v1/audio/speech',
    model: 'kokoro',
    voice: 'af_heart',
    presets: ['af_heart', 'af_bella', 'af_sky', 'am_adam', 'am_michael', 'bf_emma', 'bm_george'],
  },
  cartesia: {
    name: 'Cartesia Sonic',
    endpoint: 'https://api.cartesia.ai/tts/bytes',
    model: 'sonic-3.6',
    fixedEndpoint: true,
    voiceNote: 'Enter a voice ID from Cartesia.',
  },
  deepgram: {
    name: 'Deepgram Aura',
    endpoint: 'https://api.deepgram.com/v1/speak',
    model: 'aura-2-thalia-en',
    fixedEndpoint: true,
  },
  hermes: {
    name: 'Hermes dashboard',
    endpoint: 'http://127.0.0.1:9119/api/audio/speak',
  },
  open_ai_compatible: {
    name: 'OpenAI-compatible',
    endpoint: 'http://127.0.0.1:8880/v1/audio/speech',
  },
};

export function replyPlaybackProfile(voice, voiceInput, message) {
  if (!voice?.selected || (!voiceInput && !voice.autoplay)) return null;
  if (message?.role !== 'assistant' || message.incomplete || message.image_generation || !message.text.trim()) return null;
  return voice.selected;
}

export function speechPlayer(invoke, changed, onError) {
  let current;
  const release = (job) => {
    job.audio.onended = job.audio.onerror = null;
    job.audio.pause();
    job.audio.removeAttribute('src');
    job.audio.load();
    if (job.url) URL.revokeObjectURL(job.url);
    job.url = null;
  };

  function stop() {
    const job = current;
    if (!job) return;
    current = null;
    release(job);
    void invoke('stop_speech', { job: job.id }).catch(() => {});
    changed();
  }

  function handleError(job, error) {
    if (current !== job) return;
    stop();
    onError(String(error), true);
  }

  async function play(job) {
    try {
      await job.audio.play();
      if (current === job && job.blocked) {
        job.blocked = false;
        changed();
      }
    } catch (error) {
      if (current !== job) return;
      if (error.name !== 'NotAllowedError') {
        handleError(job, error);
        return;
      }
      job.blocked = true;
      changed();
      onError('Click Play voice to start playback.');
    }
  }

  async function load(job, part) {
    try {
      const clip = await invoke('speak', { job: job.id, profile: job.profile, source: job.source, part });
      if (current !== job) return;
      if (!clip) { stop(); return; }
      const bytes = Uint8Array.from(atob(clip.base64), (character) => character.charCodeAt(0));
      job.url = URL.createObjectURL(new Blob([bytes], { type: clip.mime }));
      job.audio.src = job.url;
      job.audio.onended = () => {
        if (current !== job) return;
        if (!clip.more) { stop(); return; }
        release(job);
        void load(job, part + 1);
      };
      job.audio.onerror = () => handleError(job, 'This audio format could not be played.');
      await play(job);
    } catch (error) {
      handleError(job, error);
    }
  }

  function start(profile, source = null) {
    stop();
    const job = { id: uuid(), profile, source, audio: new Audio(), url: null, blocked: false };
    current = job;
    changed();
    void load(job, 0);
  }

  function toggle() {
    if (!current) return;
    if (current.blocked) void play(current);
    else stop();
  }

  return { start, stop, toggle, current: () => current };
}

export function voiceSettings(api, heading, formSection, render, run) {
  let editing = null;
  let adding = false;

  return function page() {
    const { profiles = [], selected, autoplay } = api.data().voice ?? {};
    const current = profiles.find((item) => item.id === editing) ?? profiles.find((item) => item.id === selected) ?? profiles[0];
    const view = el('div', { class: 'settings-page' },
      heading('Voice', 'Choose a voice for AI-generated speech.',
        button('Add voice', () => { adding = true; render(); }, { glyph: 'plus', style: 'button primary' })));

    if (profiles.length) {
      const status = el('div', { class: 'form-status', role: 'status' });
      const active = options([['', 'Off'], ...profiles.map((profile) => [profile.id, profile.name])], selected);
      const automatic = check('Also read typed replies aloud', Boolean(autoplay), () => {});
      const autoplayInput = automatic.querySelector('input');
      autoplayInput.disabled = !selected;
      const update = async () => {
        active.disabled = autoplayInput.disabled = true;
        await run(active, status, async () => {
          api.stopSpeech();
          await api.invoke('select_voice', { selected: active.value || null, autoplay: autoplayInput.checked });
          await api.refresh();
          if (!adding) editing = active.value;
          render();
        });
        autoplayInput.disabled = !active.value;
      };
      active.addEventListener('change', update);
      autoplayInput.addEventListener('change', update);
      view.append(formSection('Reply playback', 'Voice messages get a spoken reply. OpenDex sends the assistant’s reply text to the active voice service for playback.',
        field('Active voice', active), automatic, status));
      view.append(el('div', { class: 'setting-cards' }, profiles.map((profile) =>
        button(profile.name, () => { adding = false; editing = profile.id; render(); },
          { glyph: 'speaker', style: 'connection-card' + (!adding && current?.id === profile.id ? ' selected' : '') }))));
    }
    if (adding || current) view.append(profileForm(adding ? null : current));
    return view;
  };

  function profileForm(saved) {
    const initialKind = saved?.kind ?? 'open_ai';
    const starting = adapters[initialKind];
    const id = saved?.id ?? uuid();
    const kind = options(Object.entries(adapters).map(([id, item]) => [id, item.name]), initialKind);
    const name = el('input', { value: saved?.name ?? starting.name, required: true, maxlength: 120 });
    const endpoint = el('input', { type: 'url', value: saved?.endpoint ?? starting.endpoint, required: true });
    const model = el('input', { value: saved?.model ?? starting.model ?? '', maxlength: 300 });
    const voice = el('input', { value: saved?.voice ?? starting.voice ?? '', maxlength: 1000 });
    const header = el('input', { value: saved?.key_header ?? '', placeholder: 'X-API-Key', autocomplete: 'off' });
    const key = el('input', { type: 'password', autocomplete: 'off', placeholder: saved?.has_secret ? 'Key saved' : '' });
    const forget = check('Forget saved key', false, () => {});
    const formFields = el('div', { class: 'form-grid' });
    const status = el('div', { class: 'form-status', role: 'status' });
    const save = el('button', { type: 'submit', class: 'button primary' }, 'Save voice');
    const chooser = el('div');
    const presetSelect = (items) => field('Choose a voice', options(
      [['', 'Select a voice'], ...items.map(({ id, name }) => [id, name])], '', (value) => {
        if (value) voice.value = value;
      }));

    function fields() {
      const adapter = adapters[kind.value];
      const isElevenLabs = kind.value === 'eleven_labs';
      const isPocket = kind.value === 'pocket_tts';
      const isHermes = kind.value === 'hermes';
      const isPiper = kind.value === 'piper';
      const isDeepgram = kind.value === 'deepgram';
      const usesModel = !isPocket && !isHermes;
      const usesVoice = !isHermes && !isDeepgram;
      endpoint.readOnly = Boolean(adapter.fixedEndpoint);
      model.required = usesModel && !isPiper;
      voice.required = usesVoice && !isPiper && !isPocket;
      chooser.replaceChildren();
      if (adapter.presets) chooser.append(presetSelect(adapter.presets.map((id) => ({ id, name: id }))));
      if (isElevenLabs && saved?.kind === 'eleven_labs' && saved.has_secret) {
        const query = el('input', { type: 'search', maxlength: 300, placeholder: 'Voice name' });
        const result = el('div', { role: 'status' });
        const discover = button('Find voices', () => run(discover, result, async () => {
          const list = await api.invoke('discover_voices', { id, search: query.value });
          result.replaceChildren(list.length ? presetSelect(list) : 'No matching voices.');
        }), { glyph: 'search' });
        chooser.append(el('div', { class: 'form-grid' },
          field('Search ElevenLabs voices', query), el('div', { class: 'form-actions' }, discover),
          el('div', { class: 'wide' }, result)));
      }
      formFields.replaceChildren(
        field('Connection type', kind), field('Name', name),
        field('Endpoint URL', endpoint, adapter.endpointNote ?? (isHermes ? 'Use the dashboard’s /api/audio/speak endpoint.' : isPocket ? 'Use the full /tts URL.' : '')),
        usesModel && field(isPiper ? 'Voice model' : isDeepgram ? 'Voice model ID' : 'Speech model ID', model,
          isPiper ? 'Leave blank to use the server’s loaded model.' : ''),
        usesVoice && field(isPiper ? 'Speaker name or ID' : isPocket ? 'Voice name or URL' : 'Voice ID', voice,
          adapter.voiceNote ?? (isPiper || isPocket ? 'Leave blank to use the server default.' : '')),
        chooser.childElementCount ? el('div', { class: 'wide' }, chooser) : '',
        field(isHermes ? 'Dashboard session token' : 'API key', key,
          'Saved in the system credential store.'),
        !adapter.fixedEndpoint ? field('Key header', header,
          isHermes ? 'Defaults to X-Hermes-Session-Token.' : 'Leave blank for bearer authentication.') : '',
        saved?.has_secret ? forget : '',
      );
    }
    kind.addEventListener('change', () => {
      const adapter = adapters[kind.value];
      name.value = adapter.name;
      endpoint.value = adapter.endpoint;
      model.value = adapter.model ?? '';
      voice.value = adapter.voice ?? '';
      header.value = '';
      key.value = '';
      forget.querySelector('input').checked = false;
      fields();
    });
    fields();

    const form = el('form', { onsubmit: (event) => {
      event.preventDefault();
      void run(save, status, async () => {
        api.stopSpeech();
        try {
          await api.invoke('save_voice_profile', {
            profile: {
              id, name: name.value, kind: kind.value, endpoint: endpoint.value,
              model: model.value, voice: voice.value, key_header: header.value,
            }, secret: key.value.trim() || null, forget: forget.querySelector('input').checked,
          });
        } finally {
          key.value = '';
        }
        await api.refresh();
        adding = false;
        editing = id;
        render();
      });
    } }, formFields, el('div', { class: 'form-actions' }, save,
      saved && button('Preview saved voice', () => api.previewVoice(id), { glyph: 'speaker' }),
      saved && button('Stop audio', api.stopSpeech, { glyph: 'stop' }),
      saved ? button('Remove', () => api.confirm('Remove voice', 'This removes the saved voice connection and its key.', async () => {
        api.stopSpeech();
        await api.invoke('remove_voice_profile', { id });
        await api.refresh();
        editing = null;
        render();
      }, 'Remove voice')) : button('Cancel', () => { adding = false; render(); }),
      status));
    return formSection(saved ? 'Edit connection' : 'New voice connection',
      kind.value === 'hermes' ? 'Uses the voice configured on the Hermes server.' : '', form);
  }
}
