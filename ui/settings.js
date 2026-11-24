import { $, button, check, el, field, lines, options, uuid } from './dom.js';
import { icon } from './icons.js';

const adapters = {
  ollama: ['Ollama Local', 'http://127.0.0.1:11434/api'],
  ollama_cloud: ['Ollama Cloud', 'https://ollama.com/api'],
  open_ai: ['OpenAI API', 'https://api.openai.com/v1'],
  chat_gpt_account: ['ChatGPT Account', ''],
  anthropic: ['Anthropic API', 'https://api.anthropic.com/v1'],
  open_ai_compatible: ['OpenAI-compatible', 'http://127.0.0.1:1234/v1'],
};

const sections = [
  ['providers', 'Providers', 'server'], ['mcp', 'MCP Servers', 'plug'],
  ['permissions', 'Permissions', 'shield'], ['appearance', 'Appearance', 'palette'],
  ['instructions', 'Instructions', 'note'],
];

const profiles = [
  ['read_only', 'Read only', 'Read project files and review changes.'],
  ['workspace', 'Workspace', 'Read the project and ask before edits, commands, and MCP calls.'],
  ['full_access', 'Full access', 'Allow commands with the app’s user permissions and network access. MCP calls still prompt.'],
  ['custom', 'Custom', 'Choose which actions require approval.'],
];

const profileRules = {
  read_only: { read: 'allow', write: 'deny', command: 'deny', mcp: 'deny', network: false },
  workspace: { read: 'allow', write: 'ask', command: 'ask', mcp: 'ask', network: false },
  full_access: { read: 'allow', write: 'allow', command: 'allow', mcp: 'ask', network: true },
};

export const permissionName = (key) => profiles.find(([id]) => id === key)?.[1] ?? 'Workspace';

function input(value, attrs = {}) {
  return el('input', { type: 'text', value, ...attrs });
}

function textarea(value, attrs = {}) {
  return el('textarea', { ...attrs }, value);
}

function formSection(title, description, ...children) {
  return el('section', { class: 'settings-group' },
    el('div', { class: 'section-heading' }, el('h3', {}, title),
      description && el('p', { class: 'muted' }, description)), children);
}

function heading(title, description, action) {
  return el('div', { class: 'page-heading' },
    el('div', {}, el('h2', {}, title), el('p', { class: 'muted' }, description)), action);
}

const numberOrNull = (value) => value ? Number(value) : null;

export function settingsPanel(api) {
  let tab = 'providers';
  let selectedProvider = null;
  let selectedServer = null;
  let newProviderKind = null;
  let newServer = false;
  let discovery = new Map();
  let tested = new Map();
  let accountStatus;
  let accountBusy = false;
  let authUrl;
  let catalogSearch = '';

  function open(section = 'providers', id = null) {
    if (section === 'providers' && id) selectedProvider = id;
    tab = section;
    render();
    if (!$('settings').open) $('settings').showModal();
  }

  function render() {
    $('settings-tabs').replaceChildren(...sections.map(([key, label, glyph]) =>
      button(label, () => { tab = key; render(); }, {
        glyph, style: 'settings-tab' + (tab === key ? ' current' : ''),
      })));
    const page = { providers, mcp, permissions, appearance, instructions }[tab]();
    $('settings-content').replaceChildren(page);
  }

  function makeProvider(kind) {
    return {
      id: uuid(), name: adapters[kind][0], kind, endpoint: adapters[kind][1], models: [],
      has_secret: false, headers: [], token_limit: null, ollama_think: null,
    };
  }

  function providers() {
    const data = api.data();
    const current = data.providers.find((provider) => provider.id === selectedProvider) ?? data.providers[0];
    const adding = newProviderKind !== null;
    const page = el('div', { class: 'settings-page' },
      heading('Providers', 'Connect services and keep the models you use in each picker.',
        button('Add provider', () => { newProviderKind = 'open_ai_compatible'; render(); }, { glyph: 'plus', style: 'button primary' })));

    if (data.providers.length) {
      page.append(el('div', { class: 'setting-cards' }, data.providers.map((provider) =>
        button(provider.name, () => {
          selectedProvider = provider.id; newProviderKind = null; catalogSearch = ''; render();
        }, { glyph: provider.kind === 'chat_gpt_account' ? 'chat' : 'server',
          style: 'connection-card' + (!adding && current?.id === provider.id ? ' selected' : '') }))));
    }
    if (adding) {
      page.append(providerForm(makeProvider(newProviderKind), true));
    } else if (current) {
      page.append(providerForm(current, false));
      page.append(models(current));
    } else {
      page.append(el('p', { class: 'muted' }, 'Add a provider to send your first message.'));
    }
    return page;
  }

  function providerForm(provider, adding) {
    const form = el('form', { class: 'form-grid' });
    const name = input(provider.name, { required: true, maxlength: 80 });
    const endpoint = input(provider.endpoint, { required: true, spellcheck: 'false' });
    const secret = input('', {
      type: 'password', autocomplete: 'off', placeholder: provider.has_secret ? 'Saved in system credential store' : 'Paste API key',
    });
    const tokenLimit = input(provider.token_limit, { type: 'number', min: 1, max: 1000000, placeholder: 'Adapter default' });
    let think = provider.ollama_think == null ? 'default' : String(provider.ollama_think);
    const ollama = provider.kind === 'ollama' || provider.kind === 'ollama_cloud';
    const headers = textarea(provider.headers.map(([name, variable]) => name + '=' + variable).join('\n'),
      { rows: 2, spellcheck: 'false', placeholder: 'X-Organization=MY_ORG' });
    let forget = false;
    const status = el('div', { class: 'form-status', role: 'status' });
    const isAccount = provider.kind === 'chat_gpt_account';
    form.append(
      field('Adapter', options(Object.entries(adapters).map(([key, [name]]) => [key, name]), provider.kind,
        (value) => { newProviderKind = value; render(); }, { disabled: !adding })),
      field('Display name', name),
    );
    if (!isAccount) {
      form.append(
        field('Base URL', endpoint, 'Include the API prefix, such as /v1 or /api.'),
        field('API key', secret, provider.has_secret ? 'Leave blank to keep the key for this endpoint.' : 'Optional for endpoints that accept unauthenticated requests.'),
        el('details', { class: 'wide inset' }, el('summary', {}, 'Advanced connection settings'),
          el('div', { class: 'form-grid compact' }, field('Output token limit', tokenLimit),
            field('Headers from environment', headers, 'One header per line. Use HEADER_NAME=ENVIRONMENT_VARIABLE.'),
            ollama && field('Model thinking', options([['default', 'Model default'], ['true', 'On'], ['false', 'Off']], think, (value) => { think = value; })))),
      );
      if (provider.has_secret) form.append(check('Forget saved key', false, (value) => { forget = value; }));
    }
    const saveButton = el('button', { type: 'submit', class: 'button primary' }, icon('check', 16), adding ? 'Add provider' : 'Save connection');
    const actions = el('div', { class: 'form-actions wide' }, saveButton, status);
    if (adding) {
      actions.append(button('Cancel', () => { newProviderKind = null; render(); }));
    } else {
      actions.append(button('Remove provider', () => api.confirm('Remove provider', 'This removes the connection and saved key from OpenDex.', async () => {
        if (isAccount && (accountStatus?.connected)) throw new Error('Disconnect the account before removing this provider.');
        await api.invoke('remove_provider', { id: provider.id });
        selectedProvider = null;
        await api.refresh();
        render();
      }), { style: 'button danger quiet' }));
    }
    form.append(actions);
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      await run(saveButton, status, async () => {
        const headerPairs = lines(headers.value).map((line) => {
          const at = line.indexOf('=');
          if (at < 1) throw new Error('Use HEADER_NAME=ENVIRONMENT_VARIABLE for each header.');
          return [line.slice(0, at).trim(), line.slice(at + 1).trim()];
        });
        const value = { ...provider, name: name.value, endpoint: endpoint.value, headers: headerPairs, token_limit: numberOrNull(tokenLimit.value), ollama_think: think === 'default' ? null : think === 'true' };
        const saved = await api.invoke('save_provider', { value, secret: secret.value || null, forget });
        selectedProvider = saved.id; newProviderKind = null; catalogSearch = '';
        await api.refresh();
        render();
        api.toast('Connection saved.');
      });
    });

    const section = formSection(adding ? 'New connection' : provider.name, '', form);
    if (provider.kind === 'anthropic') {
      section.append(el('p', { class: 'field-note' }, 'Claude API uses a Console key. Claude.ai subscription sign-in stays in Anthropic’s own apps.'));
    }
    if (isAccount && !adding) section.append(account());
    return section;
  }

  function account() {
    const codexPath = input(api.data().codex_path, { spellcheck: 'false', placeholder: 'codex' });
    const status = el('div', { class: 'form-status', role: 'status' }, accountStatus?.label ?? 'Check connection status');
    async function action(next) {
      if (accountBusy) return;
      accountBusy = true;
      authUrl = null;
      status.textContent = next === 'connect' ? 'Waiting for browser sign-in…' : 'Checking…';
      try {
        await api.savePreferences({ codex_path: codexPath.value });
        accountStatus = await api.invoke('account_action', { action: next });
        api.toast(next === 'connect' ? 'ChatGPT account connected.' : accountStatus.label);
      } catch (error) {
        api.toast(String(error), true);
      } finally {
        accountBusy = false;
        authUrl = null;
        if ($('settings').open && tab === 'providers') render();
      }
    }
    return el('div', { class: 'account-panel' },
      el('h4', {}, 'Sign in with ChatGPT'),
      el('p', { class: 'field-note' }, 'Uses the installed Codex CLI and your eligible ChatGPT coding models. OpenDex keeps its Codex connection separate from your terminal setup.'),
      field('Codex executable', codexPath, 'Use codex from your PATH or enter the full path.'),
      el('div', { class: 'form-actions' },
        button('Connect account', () => action('connect'), { glyph: 'arrow', style: 'button primary', disabled: accountBusy }),
        button('Check status', () => action('status'), { disabled: accountBusy }),
        button('Disconnect', () => action('disconnect'), { disabled: accountBusy }),
        status),
      authUrl && el('div', { class: 'account-url' }, 'Continue in your browser.',
        button('Copy sign-in URL', () => api.copy(authUrl), { glyph: 'copy' })),
    );
  }

  async function addModel(provider, id) {
    id = id.trim();
    if (!id || provider.models.includes(id)) return;
    await api.invoke('save_provider', { value: { ...provider, models: [...provider.models, id] }, secret: null, forget: false });
    await api.refresh();
    render();
    api.toast('Model added to the picker.');
  }

  function models(provider) {
    const isCloud = provider.kind === 'ollama_cloud';
    const status = el('div', { class: 'form-status', role: 'status' });
    const available = discovery.get(provider.id);
    const search = input(catalogSearch, { type: 'search', placeholder: isCloud ? 'Search Ollama Cloud models' : 'Filter discovered models', 'aria-label': 'Filter model catalog' });
    const results = el('div', { class: 'catalog' });
    function fillResults() {
      const text = search.value.trim().toLowerCase();
      results.replaceChildren();
      if (!available) return;
      const matches = available.filter((id) => id.toLowerCase().includes(text));
      const buttons = matches.slice(0, 55).map((id) =>
        button(id, () => addModel(provider, id).catch((error) => api.toast(String(error), true)), {
          glyph: provider.models.includes(id) ? 'check' : 'plus',
          disabled: provider.models.includes(id), style: 'catalog-result',
        }));
      results.append(el('div', { class: 'catalog-items' }, buttons));
      if (!matches.length) results.append(el('p', { class: 'muted' }, 'No matching models.'));
      if (matches.length > 55) results.append(el('p', { class: 'muted' }, `${matches.length} matches. Refine your search to see more.`));
    }
    search.addEventListener('input', () => { catalogSearch = search.value; fillResults(); });
    fillResults();
    const refresh = button(isCloud ? 'Fetch Cloud catalog' : 'Discover models', async () => {
      await run(refresh, status, async () => {
        const values = await api.invoke('discover_models', { id: provider.id });
        discovery.set(provider.id, values);
        render();
        api.toast(`Found ${values.length} model${values.length === 1 ? '' : 's'}.`);
      });
    }, { glyph: 'refresh' });
    const manual = input('', { placeholder: 'Enter a model ID', spellcheck: 'false', required: true, 'aria-label': 'Model ID' });
    const manualForm = el('form', { class: 'inline-form', onsubmit: (event) => {
      event.preventDefault();
      addModel(provider, manual.value).catch((error) => api.toast(String(error), true));
    } }, manual, el('button', { type: 'submit', class: 'button' }, icon('plus', 16), 'Add model'));
    const pinned = el('div', { class: 'pinned-models' }, provider.models.map((id) =>
      el('div', { class: 'chip' }, el('span', {}, id), button('Remove ' + id, async () => {
        try {
          await api.invoke('save_provider', { value: { ...provider, models: provider.models.filter((item) => item !== id) }, secret: null, forget: false });
          await api.refresh(); render();
        } catch (error) { api.toast(String(error), true); }
      }, { glyph: 'close', onlyIcon: true, style: 'icon-button small' }))));
    return formSection('Model picker', 'Only added models appear in Model Chat and Code Agent.',
      manualForm, pinned, el('div', { class: 'inline-form' }, available && search, refresh, status), results);
  }

  function mcp() {
    const data = api.data();
    const current = data.servers.find((server) => server.id === selectedServer) ?? data.servers[0];
    const page = el('div', { class: 'settings-page' },
      heading('MCP Servers', 'Connect tool servers and choose which tools they can expose.',
        button('Add server', () => { newServer = true; render(); }, { glyph: 'plus', style: 'button primary' })));
    if (data.servers.length) {
      page.append(el('div', { class: 'setting-cards' }, data.servers.map((server) =>
        button(server.name, () => { selectedServer = server.id; newServer = false; render(); },
          { glyph: 'plug', style: 'connection-card' + (!newServer && current?.id === server.id ? ' selected' : '') }))));
    }
    if (newServer) {
      page.append(serverForm({ id: uuid(), name: '', transport: 'stdio', address: '', args: [], env: [], has_secret: false,
        enabled: false, blocked_tools: [], current_protocol: false }, true));
    } else if (current) {
      page.append(serverForm(current, false));
    } else {
      page.append(el('div', { class: 'empty-settings' }, icon('plug', 26), el('h3', {}, 'No MCP servers'),
        el('p', { class: 'muted' }, 'Add a local command or Streamable HTTP endpoint.')));
    }
    return page;
  }

  function serverForm(server, adding) {
    const form = el('form', { class: 'form-grid' });
    const name = input(server.name, { required: true, placeholder: 'Documentation tools' });
    const address = input(server.address, { required: true, spellcheck: 'false' });
    const secret = input('', { type: 'password', autocomplete: 'off', placeholder: server.has_secret ? 'Saved bearer key' : 'Optional' });
    const args = textarea(JSON.stringify(server.args), { rows: 3, spellcheck: 'false', placeholder: '["--option", "value"]' });
    const env = textarea(server.env.join('\n'), { rows: 2, spellcheck: 'false', placeholder: 'SERVER_API_KEY' });
    const transportFields = el('div', { class: 'form-grid wide' });
    let transport = server.transport;
    let enabled = server.enabled;
    let current = server.current_protocol;
    let forget = false;
    const blocked = new Set(server.blocked_tools);
    const status = el('div', { class: 'form-status', role: 'status' });

    function displayTransport() {
      address.placeholder = transport === 'stdio' ? '/path/to/server' : 'https://example.org/mcp';
      transportFields.replaceChildren(
        field(transport === 'stdio' ? 'Executable' : 'Streamable HTTP URL', address),
        ...(transport === 'stdio'
          ? [field('Arguments as JSON', args), field('Environment variable names', env, 'Values are read from the OpenDex process environment.')]
          : [field('Bearer key', secret), server.has_secret && check('Forget saved key', forget, (value) => { forget = value; })]),
      );
    }
    const transportSelect = options([['stdio', 'Local stdio'], ['http', 'Streamable HTTP']], transport,
      (value) => { transport = value; displayTransport(); });
    displayTransport();
    const match = tested.get(server.id);
    const toolList = match && el('div', { class: 'wide mcp-tools' },
      el('h4', {}, `Tools · MCP ${match.version}`),
      match.tools.length ? match.tools.map((tool) => el('div', { class: 'tool-switch' },
        check(tool.name, !blocked.has(tool.name), (value) => value ? blocked.delete(tool.name) : blocked.add(tool.name)),
        tool.description && el('span', { class: 'muted' }, tool.description))) : el('p', { class: 'muted' }, 'This server exposes no tools.'));
    const saveButton = el('button', { type: 'submit', class: 'button primary' }, icon('check', 16), adding ? 'Add server' : 'Save server');
    const actions = el('div', { class: 'form-actions wide' }, saveButton, status);
    if (adding) actions.append(button('Cancel', () => { newServer = false; render(); }));
    else {
      const test = button('Test & list tools', async () => {
        const ready = () => run(test, status, async () => {
          const info = await api.invoke('test_server', { id: server.id });
          tested.set(server.id, info);
          render();
          api.toast(`Connected · ${info.tools.length} tools.`);
        });
        if (server.transport === 'stdio') api.confirm('Launch MCP server', `OpenDex will start the saved executable on this computer.\n${server.address}`, ready, 'Launch');
        else await ready();
      }, { glyph: 'plug' });
      actions.append(test, button('Remove server', () => api.confirm('Remove MCP server', 'This removes its saved configuration and bearer key.', async () => {
        await api.invoke('remove_server', { id: server.id }); tested.delete(server.id); selectedServer = null;
        await api.refresh(); render();
      }), { style: 'button danger quiet' }));
    }
    form.append(
      field('Display name', name), field('Transport', transportSelect), transportFields,
      check('Enable in chat and agent turns', enabled, (value) => { enabled = value; }),
      check('Use MCP 2026-07-28', current, (value) => { current = value; }),
      el('p', { class: 'field-note wide' }, 'Enabled local servers launch as your user. Only connect executables and endpoints you trust.'),
      toolList, actions,
    );
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      let parsed;
      try { parsed = transport === 'stdio' ? JSON.parse(args.value || '[]') : []; }
      catch { status.textContent = 'Enter arguments as a JSON array of strings.'; return; }
      if (!Array.isArray(parsed) || parsed.some((arg) => typeof arg !== 'string')) {
        status.textContent = 'Enter arguments as a JSON array of strings.'; return;
      }
      const save = () => run(saveButton, status, async () => {
        const value = {
          ...server, name: name.value, transport, address: address.value, args: parsed, enabled,
          env: transport === 'stdio' ? lines(env.value) : [], blocked_tools: [...blocked], current_protocol: current,
        };
        const saved = await api.invoke('save_server', { value, secret: transport === 'http' ? secret.value || null : null, forget });
        if (value.transport !== server.transport || value.address !== server.address || value.current_protocol !== server.current_protocol) {
          tested.delete(saved.id);
        }
        selectedServer = saved.id; newServer = false;
        await api.refresh(); render(); api.toast('MCP server saved.');
      });
      if (enabled && transport === 'stdio' && (!server.enabled || server.address !== address.value || JSON.stringify(server.args) !== JSON.stringify(parsed))) {
        api.confirm('Enable local MCP server', 'This executable will launch when a conversation enables MCP tools.', save, 'Enable');
      } else {
        save();
      }
    });
    return formSection(adding ? 'New server' : server.name, '', form);
  }

  function permissions() {
    const value = structuredClone(api.data().permissions);
    const page = el('div', { class: 'settings-page' }, heading('Permissions', 'Choose what Code Agent and MCP tools can do.'));
    const picker = el('div', { class: 'profile-list' });
    const rules = el('div', { class: 'form-grid' });
    const steps = input(value.max_steps, { type: 'number', min: 1, max: 50 });
    const update = () => {
      picker.replaceChildren(...profiles.map(([key, label, info]) => el('button', {
        type: 'button', class: 'profile-choice' + (value.profile === key ? ' selected' : ''), onclick: () => {
          if (key === 'custom' && value.profile !== 'custom') Object.assign(value, profileRules[value.profile]);
          value.profile = key; update();
        },
      }, el('span', { class: 'choice-radio' }), el('span', {}, el('strong', {}, label), el('span', { class: 'muted' }, info)))));
      const effective = value.profile === 'custom' ? value : profileRules[value.profile];
      rules.replaceChildren(
        ...[['read', 'Read workspace files'], ['write', 'Write workspace files'], ['command', 'Run local commands'], ['mcp', 'Use MCP tools']].map(([key, label]) =>
          field(label, options([['deny', 'Deny'], ['ask', 'Ask every time'], ['allow', 'Allow']], effective[key],
            (next) => { value[key] = next; }, { disabled: value.profile !== 'custom' }))),
        field('Maximum tool calls per turn', steps),
        value.profile === 'custom'
          ? check('Network for workspace commands', value.network, (next) => { value.network = next; })
          : el('p', { class: 'field-note' }, effective.network ? 'Command network access is on.' : 'Command network access is off.'),
      );
    };
    update();
    const save = button('Save permissions', async () => {
      const persist = async () => {
        await api.savePreferences({ permissions: { ...value, max_steps: Number(steps.value) || 16 } });
        api.toast('Permissions saved for new turns.');
      };
      if (value.profile === 'full_access') api.confirm('Enable full access', 'Approved tools can run commands with the same filesystem and network access as OpenDex.', persist, 'Enable full access');
      else await persist().catch((error) => api.toast(String(error), true));
    }, { glyph: 'check', style: 'button primary' });
    page.append(formSection('Access level', '', picker),
      formSection('Granular controls', 'Choose Custom to set a separate rule for each action. Scoped command isolation is available on macOS.',
        rules, el('div', { class: 'form-actions' }, save)));
    return page;
  }

  function appearance() {
    const value = structuredClone(api.data().appearance);
    const page = el('div', { class: 'settings-page' }, heading('Appearance', 'Adjust the look and spacing of your workspace.'));
    const update = (key, next) => { value[key] = next; api.previewAppearance(value); };
    function select(label, key, items) {
      return field(label, options(items, value[key], (next) => update(key, next)));
    }
    function slider(label, key, min, max, suffix) {
      const display = el('output', {}, '' + value[key] + suffix);
      return field(label, el('div', { class: 'range-field' }, el('input', {
        type: 'range', min, max, value: value[key], oninput: (event) => {
          update(key, Number(event.target.value)); display.textContent = event.target.value + suffix;
        },
      }), display));
    }
    const accent = input(value.accent, { type: 'color', oninput: (event) => update('accent', event.target.value) });
    page.append(formSection('Interface', '',
      el('div', { class: 'form-grid' },
        select('Color mode', 'theme', [['dark', 'Dark'], ['light', 'Light'], ['system', 'Match system']]),
        field('Accent color', accent),
        select('Typeface', 'font', [['system', 'System'], ['mono', 'Monospace'], ['serif', 'Serif']]),
        select('Spacing', 'density', [['compact', 'Compact'], ['comfortable', 'Comfortable'], ['spacious', 'Spacious']]),
        slider('Text scale', 'font_scale', 80, 140, '%'), slider('Conversation width', 'width', 600, 1400, ' px'),
        slider('Sidebar width', 'sidebar', 185, 355, ' px'), slider('Corner radius', 'radius', 0, 28, ' px'),
        check('Use animations', value.motion, (next) => update('motion', next)),
      ), el('div', { class: 'form-actions' },
        button('Save appearance', async () => {
          try { await api.savePreferences({ appearance: value }); api.toast('Appearance saved.'); }
          catch (error) { api.toast(String(error), true); }
        }, { glyph: 'check', style: 'button primary' }),
        button('Reset defaults', async () => {
          const defaults = { theme: 'dark', accent: '#c1f483', font_scale: 100, density: 'comfortable', font: 'system', width: 850, sidebar: 252, radius: 15, motion: true };
          await api.savePreferences({ appearance: defaults }); render();
        }))));
    return page;
  }

  function instructions() {
    const box = textarea(api.data().instructions, { rows: 9, placeholder: 'Add preferences for responses and workflow…', maxlength: 32000 });
    return el('div', { class: 'settings-page' }, heading('Instructions', 'Set preferences sent with Model Chat and Code Agent requests.'),
      formSection('Global instructions', 'These apply to all conversations. You can add thread instructions from its menu.',
        field('Instructions', box), el('div', { class: 'form-actions' },
          button('Save instructions', async () => {
            try { await api.savePreferences({ instructions: box.value }); api.toast('Instructions saved.'); }
            catch (error) { api.toast(String(error), true); }
          }, { glyph: 'check', style: 'button primary' }))));
  }

  async function run(button, status, operation) {
    button.disabled = true;
    status.textContent = 'Working…';
    status.classList.remove('error');
    try { await operation(); }
    catch (error) { status.classList.add('error'); status.textContent = String(error); }
    finally { button.disabled = false; }
  }

  $('close-settings').addEventListener('click', () => $('settings').close());
  $('settings').addEventListener('close', () => api.previewAppearance(api.data().appearance));

  return { open, accountConnection: (event) => {
    authUrl = event.url;
    api.toast(event.error ? 'Copy the sign-in link from Provider settings.' : 'Finish sign-in in your browser.', Boolean(event.error));
    if ($('settings').open && tab === 'providers') render();
  } };
}
