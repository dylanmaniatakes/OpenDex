import { button, check, el, field, lines, options, uuid } from './dom.js';

const presets = {
  openai: { label: 'OpenAI', kind: 'open_ai', endpoint: 'https://api.openai.com/v1/images/generations', model: 'gpt-image-2.5-sunburst' },
  compatible: { label: 'OpenAI-compatible', kind: 'open_ai', endpoint: 'http://127.0.0.1:7860/v1/images/generations' },
  openrouter: { label: 'OpenRouter', kind: 'open_ai', endpoint: 'https://openrouter.ai/api/v1/images', model: 'google/gemini-3.1-flash-image', note: 'Choose a model listed in the OpenRouter Image API catalog.' },
  xai: { label: 'xAI Grok Imagine', kind: 'open_ai', endpoint: 'https://api.x.ai/v1/images/generations', model: 'grok-imagine-image-2.0', asset_hosts: ['imgen.x.ai'] },
  together: { label: 'Together AI', kind: 'open_ai', endpoint: 'https://api.together.ai/v1/images/generations', model: 'black-forest-labs/FLUX.2-dev', asset_hosts: ['*.together.ai', '*.together.xyz'] },
  recraft: { label: 'Recraft', kind: 'open_ai', endpoint: 'https://external.api.recraft.ai/v1/images/generations', model: 'recraftv4', options: { response_format: 'b64_json' } },
  gemini: { label: 'Google Gemini', kind: 'gemini', endpoint: 'https://generativelanguage.googleapis.com/v1beta', model: 'gemini-3.1-flash-image' },
  imagen: { label: 'Google Imagen', kind: 'imagen', endpoint: 'https://generativelanguage.googleapis.com/v1beta', model: 'imagen-4.0-generate-001' },
  stability: { label: 'Stability AI', kind: 'stability', endpoint: 'https://api.stability.ai/v2beta/stable-image/generate/core', options: { output_format: 'png' } },
  ideogram: { label: 'Ideogram', kind: 'ideogram', endpoint: 'https://api.ideogram.ai/v1/ideogram-v3/generate' },
  bfl: { label: 'Black Forest Labs FLUX', kind: 'black_forest_labs', endpoint: 'https://api.bfl.ai/v1/flux-2-pro' },
  fal: { label: 'fal', kind: 'fal', endpoint: 'https://queue.fal.run/fal-ai/flux/schnell' },
  replicate: { label: 'Replicate', kind: 'replicate', endpoint: 'https://api.replicate.com/v1', model: 'black-forest-labs/flux-schnell' },
  firefly: { label: 'Adobe Firefly', kind: 'firefly', endpoint: 'https://firefly-api.adobe.io/v3/images/generate-async',
    headers: [['x-api-key', 'FIREFLY_SERVICES_CLIENT_ID']], note: 'Use a current Adobe access token and supply your client ID in the environment-backed header.' },
  comfy: { label: 'ComfyUI', kind: 'comfy_ui', endpoint: 'http://127.0.0.1:8188' },
  automatic: { label: 'AUTOMATIC1111 / Forge', kind: 'custom', endpoint: 'http://127.0.0.1:7860/sdapi/v1/txt2img',
    options: { prompt: '{{prompt}}' }, output_pointer: '/images/0' },
  custom: { label: 'Custom JSON endpoint', kind: 'custom', endpoint: '', options: { prompt: '{{prompt}}' } },
};
const formats = [...new Map(Object.values(presets).map(({ kind, label }) => [kind, [kind, label]])).values()]
  .map(([kind, label]) => [kind, kind === 'open_ai' ? 'Images API' : label]);
const modelKinds = ['open_ai', 'gemini', 'imagen', 'replicate'];
const baseKinds = ['gemini', 'imagen', 'replicate', 'comfy_ui'];

const input = (value, attrs = {}) => el('input', { type: 'text', value, ...attrs });

export function imageSettings(api, heading, formSection, render, run) {
  let editing = null;
  let adding = false;

  return function page() {
    const data = api.data();
    const sources = data.image_sources ?? [];
    const ready = sources.filter((source) => source.kind !== 'comfy_ui' || Object.keys(source.options ?? {}).length);
    const selected = ready.find((source) => source.id === data.image_source_default)?.id ?? ready[0]?.id;
    const current = sources.find((item) => item.id === editing)
      ?? sources.find((item) => item.id === selected) ?? sources[0];
    const view = el('div', { class: 'settings-page' },
      heading('Images', 'Connect an image service or a local workflow.',
        button('Add image source', () => { adding = true; render(); }, { glyph: 'plus', style: 'button primary' })));

    if (ready.length) {
      const defaultPicker = options(ready.map((item) => [item.id, item.name]), selected);
      const status = el('div', { class: 'form-status', role: 'status' });
      defaultPicker.addEventListener('change', () => run(defaultPicker, status, async () => {
        await api.invoke('select_image_source', { id: defaultPicker.value });
        await api.refresh();
        render();
      }));
      view.append(formSection('Default source', 'The agent uses this source when you ask for an image without naming one.',
        field('Image source', defaultPicker), status));
    }
    if (sources.length) {
      view.append(el('div', { class: 'setting-cards' }, sources.map((source) =>
        button(source.name, () => { adding = false; editing = source.id; render(); },
          { glyph: 'image', style: 'connection-card' + (!adding && current?.id === source.id ? ' selected' : '') }))));
    }
    if (adding || current) view.append(sourceForm(adding ? null : current));
    return view;
  };

  function sourceForm(saved) {
    const id = saved?.id ?? uuid();
    const preset = options([['', 'Choose a service'], ...Object.entries(presets).map(([key, item]) => [key, item.label])], '');
    const kind = options(formats, saved?.kind ?? 'open_ai');
    const name = input(saved?.name ?? '', { required: true, maxlength: 100 });
    const address = input(saved?.endpoint ?? '', { type: 'url', required: true });
    const model = input(saved?.model ?? '', { maxlength: 300, spellcheck: 'false' });
    const secret = input('', { type: 'password', autocomplete: 'off', placeholder: saved?.has_secret ? 'Key saved' : '' });
    const keyHeader = input(saved?.key_header ?? '', { placeholder: 'Adapter default', spellcheck: 'false' });
    const headers = el('textarea', { rows: 2, spellcheck: 'false', placeholder: 'X-Organization=IMAGE_ORG' },
      (saved?.headers ?? []).map(([key, value]) => key + '=' + value).join('\n'));
    const assetHosts = el('textarea', { rows: 2, spellcheck: 'false', placeholder: 'cdn.example.com' },
      (saved?.asset_hosts ?? []).join('\n'));
    const pointer = input(saved?.output_pointer ?? '', { placeholder: '/data/0/url', spellcheck: 'false' });
    const json = el('textarea', { rows: 8, spellcheck: 'false', maxlength: 512 * 1024 }, JSON.stringify(saved?.options ?? {}, null, 2));
    const workflow = el('input', { type: 'file', accept: '.json,application/json' });
    const promptNode = el('div');
    const library = el('div', { class: 'form-grid' });
    const status = el('div', { class: 'form-status', role: 'status' });
    const note = el('p', { class: 'field-note' });
    const save = el('button', { type: 'submit', class: 'button primary' }, 'Save image source');
    let forget = false;

    function showPromptNodes(graph) {
      const nodes = Object.entries(graph).flatMap(([id, node]) =>
        Object.entries(node?.inputs ?? {}).filter(([, value]) => typeof value === 'string')
          .map(([key]) => ({ id, key, label: (node._meta?.title ?? node.class_type ?? id) + ' · ' + key + ' (' + id + ')' })));
      promptNode.replaceChildren(nodes.length ? field('Workflow prompt input', options(
        [['', 'Choose the positive prompt input'], ...nodes.map(({ label }, index) => [String(index), label])], '',
        (value) => {
          if (value === '') return;
          const { id, key } = nodes[Number(value)];
          try {
            const current = JSON.parse(json.value);
            current[id].inputs[key] = '{{prompt}}';
            json.value = JSON.stringify(current, null, 2);
          } catch (error) {
            status.textContent = String(error);
          }
        })) : '');
    }
    function useWorkflow(imported) {
      const graph = imported?.prompt ?? imported;
      if (!graph || !Object.values(graph).length || !Object.values(graph).every((node) => node?.class_type && node.inputs)) {
        throw new Error('Import an API-format workflow. In ComfyUI choose Export (API).');
      }
      json.value = JSON.stringify(graph, null, 2);
      showPromptNodes(graph);
      status.textContent = 'Workflow loaded. Choose its positive prompt input.';
    }

    workflow.addEventListener('change', async () => {
      try {
        const file = workflow.files?.[0];
        if (!file) return;
        if (file.size > 512 * 1024) throw new Error('The workflow exceeds 512 KiB.');
        useWorkflow(JSON.parse(await file.text()));
      } catch (error) {
        status.textContent = String(error);
      }
    });
    if (saved?.kind === 'comfy_ui') {
      const files = options([['', 'Browse saved server workflows']], '');
      const preview = el('pre', { class: 'wide', hidden: true });
      const browse = button('List workflows', () => run(browse, status, async () => {
        const paths = await api.invoke('list_comfy_workflows', { id });
        files.replaceChildren(el('option', { value: '' }, paths.length ? 'Choose a workflow' : 'No workflows found'),
          ...paths.map((path) => el('option', { value: path }, path)));
        preview.hidden = true;
        inspect.disabled = true;
        status.textContent = '';
      }), { glyph: 'folder' });
      const inspect = button('Open workflow', () => run(inspect, status, async () => {
        const raw = await api.invoke('read_comfy_workflow', { id, path: files.value });
        try {
          useWorkflow(raw);
          preview.hidden = true;
        } catch {
          preview.textContent = JSON.stringify(raw, null, 2);
          preview.hidden = false;
          status.textContent = 'Editor workflow. Export it in API format from ComfyUI to use for generation.';
        }
      }), { disabled: true });
      files.addEventListener('change', () => { inspect.disabled = !files.value; preview.hidden = true; });
      library.append(field('Server workflow', files), el('div', { class: 'form-actions' }, browse, inspect), preview);
    }

    const modelField = field('Model ID', model);
    const addressLabel = el('span', { class: 'field-label' });
    const addressField = el('label', { class: 'field' }, addressLabel, address);
    const jsonLabel = el('span', { class: 'field-label' });
    const jsonHelp = el('span', { class: 'field-note' });
    const jsonField = el('label', { class: 'field wide' }, jsonLabel, json, jsonHelp);
    const importFields = el('div', { class: 'wide' }, library, field('Import ComfyUI workflow', workflow), promptNode);
    const update = () => {
      model.required = modelKinds.includes(kind.value);
      modelField.hidden = !model.required;
      addressLabel.textContent = baseKinds.includes(kind.value) ? 'API base URL' : 'Generation endpoint URL';
      const comfy = kind.value === 'comfy_ui';
      const custom = kind.value === 'custom';
      importFields.hidden = !comfy;
      jsonLabel.textContent = comfy ? 'Workflow JSON' : custom ? 'JSON request body' : 'Provider options (JSON)';
      jsonHelp.textContent = comfy
        ? 'Save an empty workflow to connect the server for browsing and authoring. To generate here, use API format, mark the positive input with {{prompt}}, and include a Save Image node.'
        : custom ? 'Use {{prompt}} in a string field. A blank object sends a single prompt field.'
          : 'Optional model settings such as size, aspect ratio, or seed. Use the provider’s field names.';
    };
    preset.addEventListener('change', () => {
      const choice = presets[preset.value];
      if (!choice) return;
      name.value = choice.label;
      kind.value = choice.kind;
      address.value = choice.endpoint;
      model.value = choice.model ?? '';
      secret.value = '';
      keyHeader.value = choice.key_header ?? '';
      headers.value = (choice.headers ?? []).map(([key, value]) => key + '=' + value).join('\n');
      assetHosts.value = (choice.asset_hosts ?? []).join('\n');
      json.value = JSON.stringify(choice.options ?? {}, null, 2);
      pointer.value = choice.output_pointer ?? '';
      note.textContent = choice.note ?? '';
      promptNode.replaceChildren();
      update();
    });
    kind.addEventListener('change', update);
    update();

    const form = el('form', { class: 'form-grid', onsubmit: (event) => {
      event.preventDefault();
      void run(save, status, async () => {
        const params = JSON.parse(json.value || '{}');
        if (!params || Array.isArray(params) || typeof params !== 'object') throw new Error('Enter a JSON object.');
        const pairs = lines(headers.value).map((line) => {
          const split = line.indexOf('=');
          if (split < 1) throw new Error('Use HEADER_NAME=ENVIRONMENT_VARIABLE for each header.');
          return [line.slice(0, split).trim(), line.slice(split + 1).trim()];
        });
        try {
          await api.invoke('save_image_source', {
            source: {
              id, kind: kind.value, name: name.value, endpoint: address.value, model: model.required ? model.value : '',
              options: params, output_pointer: pointer.value, key_header: keyHeader.value,
              headers: pairs, asset_hosts: lines(assetHosts.value),
            },
            secret: secret.value.trim() || null, forget,
          });
        } finally { secret.value = ''; }
        await api.refresh();
        editing = id;
        adding = false;
        render();
      });
    } },
    !saved && field('Service preset', preset), field('API format', kind),
    field('Name', name), addressField, modelField,
    field('API key or access token', secret, 'Saved in the system credential store.'),
    importFields, jsonField,
    el('details', { class: 'wide inset' }, el('summary', {}, 'Advanced request settings'),
      el('div', { class: 'form-grid' },
        field('Key header', keyHeader, 'A custom header receives the saved key as entered.'),
        field('Headers from environment', headers, 'Use HEADER_NAME=ENVIRONMENT_VARIABLE.'),
        field('Allowed HTTPS asset hosts', assetHosts, 'One host per line. Use *.example.com for subdomains.'),
        field('Response JSON Pointer', pointer, 'Point to an image URL or Base64 string when the endpoint uses a custom response.'))),
    saved?.has_secret && check('Forget saved key', false, (value) => { forget = value; }),
    note,
    el('div', { class: 'form-actions wide' }, save,
      saved ? button('Remove source', () => api.confirm('Remove image source', 'This removes the saved connection and its key.', async () => {
        await api.invoke('remove_image_source', { id });
        await api.refresh();
        editing = null;
        render();
      }, 'Remove source')) : button('Cancel', () => { adding = false; render(); }),
      status));
    return formSection(saved ? 'Edit image source' : 'New image source', '', form);
  }
}
