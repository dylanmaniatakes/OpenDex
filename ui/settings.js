import { $, button, check, el, field, lines, options, uuid } from './dom.js';
import { backupForms } from './backup-settings.js';
import { icon } from './icons.js';

const adapters = {
  ollama: ['Ollama Local', 'http://127.0.0.1:11434/api'],
  ollama_cloud: ['Ollama Cloud', 'https://ollama.com/api'],
  open_ai: ['OpenAI API', 'https://api.openai.com/v1'],
  chat_gpt_account: ['ChatGPT Account', ''],
  anthropic: ['Anthropic API', 'https://api.anthropic.com/v1'],
  gemini: ['Google Gemini API', 'https://generativelanguage.googleapis.com/v1beta'],
  open_ai_compatible: ['OpenAI-compatible', 'http://127.0.0.1:1234/v1'],
};

const sections = [
  ['providers', 'Providers', 'server'], ['mcp', 'MCP Servers', 'plug'],
  ['web', 'Web search', 'search'],
  ['permissions', 'Permissions', 'shield'], ['appearance', 'Appearance', 'palette'],
  ['instructions', 'Instructions', 'note'],
  ['backups', 'Backups', 'shield'],
];

const profiles = [
  ['read_only', 'Read only', 'Read project files and review changes.'],
  ['workspace', 'Workspace', 'Read the project and ask before edits, commands, MCP calls, and web requests.'],
  ['full_access', 'Full access', 'Allow commands with the app’s user permissions and network access. MCP calls and web requests still prompt.'],
  ['custom', 'Custom', 'Choose which actions require approval.'],
];

const profileRules = {
  read_only: { read: 'allow', write: 'deny', command: 'deny', mcp: 'deny', web: 'deny', network: false },
  workspace: { read: 'allow', write: 'ask', command: 'ask', mcp: 'ask', web: 'ask', network: false },
  full_access: { read: 'allow', write: 'allow', command: 'allow', mcp: 'ask', web: 'ask', network: true },
};

const approvalOptions = [['ask', 'Ask every time'], ['deny', 'Deny']];

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
    const page = { providers, mcp, web, permissions, appearance, instructions, backups }[tab]();
    $('settings-content').replaceChildren(page);
  }

  function makeProvider(kind) {
    return {
      id: uuid(), name: adapters[kind][0], kind, endpoint: adapters[kind][1], models: [],
      has_secret: false, headers: [], token_limit: null, ollama_think: null,
      anthropic_auth: 'api',
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
    const defaultKeyHeader = { anthropic: 'x-api-key', gemini: 'x-goog-api-key' }[provider.kind] ?? 'Authorization: Bearer';
    const keyHeader = input(provider.api_key_header ?? '', { spellcheck: 'false', placeholder: defaultKeyHeader, maxlength: 128 });
    const tokenLimit = input(provider.token_limit, { type: 'number', min: 1, max: 1000000, placeholder: 'Adapter default' });
    let think = provider.ollama_think == null ? 'default' : String(provider.ollama_think);
    const ollama = provider.kind === 'ollama' || provider.kind === 'ollama_cloud';
    const headers = textarea(provider.headers.map(([name, variable]) => name + '=' + variable).join('\n'),
      { rows: 2, spellcheck: 'false', placeholder: 'X-Organization=MY_ORG' });
    let forget = false;
    const status = el('div', { class: 'form-status', role: 'status' });
    const isAccount = provider.kind === 'chat_gpt_account';
    const anthropic = provider.kind === 'anthropic';
    const claudePath = input(provider.claude_path ?? 'claude', { spellcheck: 'false', placeholder: 'claude' });
    let anthropicAuth = provider.anthropic_auth ?? 'api';
    form.append(
      field('Adapter', options(Object.entries(adapters).map(([key, [name]]) => [key, name]), provider.kind,
        (value) => { newProviderKind = value; render(); }, { disabled: !adding })),
      field('Display name', name),
    );
    const connectionFields = el('div', { class: 'form-grid wide' });
    function showConnectionFields() {
      connectionFields.replaceChildren();
      if (isAccount) return;
      if (anthropic && anthropicAuth === 'account') {
        connectionFields.append(field('Claude Code executable', claudePath, 'Install the unmodified Claude Code CLI. Save the connection before signing in.'));
        return;
      }
      connectionFields.append(
        field('Base URL', endpoint, 'Include the API prefix, such as /v1 or /api.'),
        field('API key', secret, provider.has_secret ? 'Leave blank to keep the key for this endpoint.' : 'Optional for endpoints that accept unauthenticated requests.'),
        el('details', { class: 'wide inset' }, el('summary', {}, 'Advanced connection settings'),
          el('div', { class: 'form-grid compact' }, field('Output token limit', tokenLimit),
            field('API key header', keyHeader, 'Leave blank for the adapter default. A custom header receives the saved key as entered.'),
            field('Headers from environment', headers, 'One header per line. Use HEADER_NAME=ENVIRONMENT_VARIABLE.'),
            ollama && field('Model thinking', options([['default', 'Model default'], ['true', 'On'], ['false', 'Off']], think, (value) => { think = value; })))),
      );
      if (provider.has_secret) connectionFields.append(check('Forget saved key', forget, (value) => { forget = value; }));
    }
    showConnectionFields();
    form.append(connectionFields);
    if (anthropic) {
      form.append(field('Anthropic access', options([['api', 'Console API key'], ['account', 'Claude account']], anthropicAuth,
        (value) => {
          anthropicAuth = value;
          showConnectionFields();
          showAccountPanel();
        })));
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
        const headerPairs = (anthropic && anthropicAuth === 'account' ? [] : lines(headers.value)).map((line) => {
          const at = line.indexOf('=');
          if (at < 1) throw new Error('Use HEADER_NAME=ENVIRONMENT_VARIABLE for each header.');
          return [line.slice(0, at).trim(), line.slice(at + 1).trim()];
        });
        const valuePatch = {
          name: name.value,
          endpoint: anthropic && anthropicAuth === 'account' ? adapters.anthropic[1] : endpoint.value,
          api_key_header: anthropic && anthropicAuth === 'account' ? null : keyHeader.value.trim() || null,
          headers: anthropic && anthropicAuth === 'account' ? [] : headerPairs,
          token_limit: numberOrNull(tokenLimit.value),
          ollama_think: think === 'default' ? null : think === 'true',
          anthropic_auth: anthropic ? anthropicAuth : 'api',
          claude_path: claudePath.value,
        };
        const value = { ...provider, ...valuePatch };
        const saved = await api.invoke('save_provider', { value, secret: anthropic && anthropicAuth === 'account' ? null : secret.value || null, forget });
        selectedProvider = saved.id; newProviderKind = null; catalogSearch = '';

        await api.refresh();
        render();
        api.toast('Connection saved.');
      });
    });

    const section = formSection(adding ? 'New connection' : provider.name, '', form);
    const accountPanel = el('div', { class: 'wide' });
    function showAccountPanel() {
      accountPanel.replaceChildren();
      if (anthropic && (anthropicAuth === 'account' || (provider.anthropic_auth === 'account' && provider.has_secret))) {
        accountPanel.append(adding ? el('p', { class: 'field-note' }, 'Save the connection to sign in.') : anthropicAccount(provider));
      }
    }
    showAccountPanel();
    section.append(accountPanel);
    if (isAccount && !adding) section.append(account());
    return section;
  }

  function anthropicAccount(provider) {
    const status = el('div', { class: 'form-status', role: 'status' }, 'Checking Claude account…');
    const actions = el('div', { class: 'form-actions' });
    const savedAsAccount = provider.anthropic_auth === 'account' && provider.kind === 'anthropic';
    if (!savedAsAccount) {
      status.textContent = 'Save this Anthropic connection first.';
    } else {
      const connect = button('Connect account', async () => {
        await run(connect, status, async () => {
          status.textContent = 'Waiting for browser sign-in…';
          await api.invoke('anthropic_account_connect', { id: provider.id });
          await api.refresh();
          render();
          api.toast('Claude Code account connected.');
        });
      }, { glyph: 'arrow', style: 'button primary', disabled: provider.has_secret });
      const disconnect = button('Sign out', async () => {
        await run(disconnect, status, async () => {
          await api.invoke('anthropic_account_disconnect', { id: provider.id });
          await api.refresh();
          render();
        });
      }, { disabled: !provider.has_secret });
      actions.append(connect, disconnect);
      api.invoke('anthropic_account_status', { id: provider.id }).then((result) => {
        const details = [result.email, result.organization, result.subscription].filter(Boolean).join(' · ');
        status.textContent = result.connected ? ['Signed in.', details, result.error].filter(Boolean).join(' ') : 'Not signed in.';
        connect.disabled = result.connected || provider.has_secret;
        disconnect.disabled = !result.connected && !provider.has_secret;
        if (provider.has_secret) status.textContent = 'Sign out of the old account prototype, then reconnect through Claude Code.';
      }).catch((error) => { status.textContent = String(error); });
    }
    actions.append(status);
    return el('div', { class: 'account-panel' },
      el('h4', {}, 'Sign in with Claude'),
      el('p', { class: 'field-note' }, 'Claude Code opens its own browser sign-in and manages the account credentials. OpenDex uses a dedicated CLI configuration for this connection.'),
      actions);
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

  async function addModels(provider, ids) {
    const models = [...new Set(ids.map((id) => id.trim()).filter(Boolean))];
    if (!models.length) throw new Error('Enter at least one model ID.');
    await api.invoke('add_provider_models', { id: provider.id, models });
    await api.refresh();
    render();
    api.toast('Model picker updated.');
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
        button(id, () => addModels(provider, [id]).catch((error) => api.toast(String(error), true)), {
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
    const manual = textarea('', { rows: 3, placeholder: 'Model IDs, one per line', spellcheck: 'false', required: true, 'aria-label': 'Model IDs' });
    const add = el('button', { type: 'submit', class: 'button' }, icon('plus', 16), 'Add models');
    const manualForm = el('form', { class: 'details-form', onsubmit: async (event) => {
      event.preventDefault();
      await run(add, status, () => addModels(provider, lines(manual.value)));
    } }, manual, el('div', { class: 'form-actions' }, add));
    const pinned = el('div', { class: 'pinned-models' }, provider.models.map((id) =>
      el('div', { class: 'chip' }, el('span', {}, id), button('Remove ' + id, async () => {
        try {
          await api.invoke('remove_provider_model', { id: provider.id, model: id });
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
    const headers = textarea((server.headers ?? []).map(([name, variable]) => name + '=' + variable).join('\n'), { rows: 2, spellcheck: 'false', placeholder: 'X-Organization=MY_ORG' });
    const transportFields = el('div', { class: 'form-grid wide' });
    let transport = server.transport;
    let enabled = server.enabled;
    let current = server.current_protocol;
    let forget = false;
    let toolPermission = server.tool_permission ?? '';
    // Keep the legacy deny switches when migrating a configuration to rule selectors.
    const toolPermissions = new Map(Object.entries(server.tool_permissions ?? {}));
    for (const name of server.blocked_tools ?? []) toolPermissions.set(name, 'deny');
    const status = el('div', { class: 'form-status', role: 'status' });

    function displayTransport() {
      address.placeholder = transport === 'stdio' ? '/path/to/server' : 'https://example.org/mcp';
      transportFields.replaceChildren(
        field(transport === 'stdio' ? 'Executable' : 'Streamable HTTP URL', address),
        ...(transport === 'stdio'
          ? [field('Arguments as JSON', args), field('Environment variable names', env, 'Values are read from the OpenDex process environment.')]
          : [field('Bearer key', secret), field('Headers from environment', headers, 'One header per line. Use HEADER_NAME=ENVIRONMENT_VARIABLE.'), ...(server.has_secret
            ? [check('Forget saved key', forget, (value) => { forget = value; })] : [])]),
      );
    }
    const transportSelect = options([['stdio', 'Local stdio'], ['http', 'Streamable HTTP']], transport,
      (value) => { transport = value; displayTransport(); });
    displayTransport();
    const match = tested.get(server.id);
    const tools = new Map((match?.tools ?? []).map((tool) => [tool.name, tool.description]));
    for (const name of toolPermissions.keys()) {
      if (!tools.has(name)) tools.set(name, '');
    }
    const toolList = (match || tools.size > 0) && el('div', { class: 'wide mcp-tools' },
      el('h4', {}, match ? `Tools · MCP ${match.version}` : 'Saved tool permissions'),
      tools.size ? Array.from(tools, ([name, description]) => el('div', { class: 'mcp-tool-rule' },
        field(name, options([['', 'Inherit server rule'], ...approvalOptions], toolPermissions.get(name),
          (value) => value ? toolPermissions.set(name, value) : toolPermissions.delete(name)), description)))
        : el('p', { class: 'muted' }, 'This server exposes no tools.'));
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
      field('Server tool approvals', options([['', 'Inherit permission profile'], ...approvalOptions], toolPermission,
        (value) => { toolPermission = value; }), 'The strictest rule applies to model tool calls.'),
      el('p', { class: 'field-note wide' }, 'Enabled local servers launch as your user. Only connect executables and endpoints you trust.'),
      toolList, actions,
    );
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      let parsed;
      let headerPairs;
      try { parsed = transport === 'stdio' ? JSON.parse(args.value || '[]') : []; }
      catch { status.textContent = 'Enter arguments as a JSON array of strings.'; return; }
      if (!Array.isArray(parsed) || parsed.some((arg) => typeof arg !== 'string')) {
        status.textContent = 'Enter arguments as a JSON array of strings.'; return;
      }
      const save = () => run(saveButton, status, async () => {
        const value = {
          ...server, name: name.value, transport, address: address.value, args: parsed, enabled,
          env: transport === 'stdio' ? lines(env.value) : [], current_protocol: current, blocked_tools: [],
          tool_permission: toolPermission || null, tool_permissions: Object.fromEntries(toolPermissions),
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

  function web() {
    const data = api.data();
    const saved = data.web_search;
    let service = saved?.kind ?? '';
    const cloud = data.providers.filter((provider) => provider.kind === 'ollama_cloud');
    const endpoint = input(saved?.endpoint ?? '', { type: 'url', required: true, placeholder: 'http://127.0.0.1:8080/search', spellcheck: 'false' });
    const provider = options(cloud.map((item) => [item.id, item.name]), saved?.provider ?? cloud[0]?.id);
    const details = el('div', { class: 'wide' });
    const status = el('div', { class: 'form-status', role: 'status' });
    const save = el('button', { type: 'submit', class: 'button primary' }, 'Save web search');
    const test = button('Test search', async () => {
      if (service === 'searxng' && !endpoint.reportValidity()) return;
      const value = connection();
      await run(test, status, async () => {
        const count = await api.invoke('test_web_search', { value });
        status.textContent = `Search service responded. ${count} ${count === 1 ? 'source' : 'sources'} returned.`;
      });
    }, { glyph: 'search', title: 'Send a test search for OpenDex' });

    function connection() {
      if (service === 'searxng') return { kind: service, endpoint: endpoint.value.trim() };
      if (service === 'ollama') return { kind: service, provider: provider.value };
      return null;
    }

    function showService() {
      details.replaceChildren();
      if (service === 'searxng') details.append(field('SearXNG search URL', endpoint, 'Use a search endpoint with the JSON format enabled.'));
      if (service === 'ollama') {
        if (cloud.length) details.append(field('Ollama Cloud connection', provider, 'OpenDex uses the endpoint and credentials from this provider.'));
        else details.append(button('Set up Ollama Cloud', () => {
          newProviderKind = 'ollama_cloud';
          open('providers');
        }, { glyph: 'plus' }));
      }
      save.disabled = service === 'ollama' && !cloud.length;
      test.disabled = !service || save.disabled;
      status.textContent = '';
    }

    const choice = options([['', 'Off'], ['searxng', 'SearXNG'], ['ollama', 'Ollama Cloud']], service, (value) => {
      service = value;
      showService();
    });
    showService();
    const form = el('form', { class: 'form-grid', onsubmit: (event) => {
      event.preventDefault();
      run(save, status, async () => {
        await api.invoke('save_web_search', { value: connection() });
        await api.refresh();
        status.textContent = 'Web search settings saved.';
      });
    } }, field('Search service', choice), details,
    el('div', { class: 'form-actions wide' }, save, test, status));
    return el('div', { class: 'settings-page' }, heading('Web search', 'Choose the service that receives search queries from your conversations.'),
      formSection('Connection', 'Turn on Web search in a conversation’s composer to make it available to the model.', form));
  }

  function permissions() {
    const value = structuredClone(api.data().permissions);
    const page = el('div', { class: 'settings-page' }, heading('Permissions', 'Choose which tools can act and when they ask for approval.'));
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
        ...[['read', 'Read workspace files'], ['write', 'Write workspace files'], ['command', 'Run local commands'], ['mcp', 'Use MCP servers'], ['web', 'Search and fetch web pages']].map(([key, label]) =>
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

  function backups() {
    const forms = backupForms(api, formSection, run, permissionName, async () => {
      selectedProvider = null;
      selectedServer = null;
      newProviderKind = null;
      newServer = false;
      discovery.clear();
      tested.clear();
      accountStatus = undefined;
      authUrl = undefined;
      await api.refresh();
    });
    return el('div', { class: 'settings-page' },
      heading('Backups', 'Move your connections, keys, and settings between computers.'), forms);
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
  $('settings').addEventListener('close', () => {
    if (tab === 'backups') $('settings-content').replaceChildren();
    api.previewAppearance(api.data().appearance);
  });

  return { open, accountConnection: (event) => {
    authUrl = event.url;
    api.toast(event.error ? 'Copy the sign-in link from Provider settings.' : 'Finish sign-in in your browser.', Boolean(event.error));
    if ($('settings').open && tab === 'providers') render();
  } };
}
