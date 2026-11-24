import { $, button, check, el, field, markdown, options } from './dom.js';
import { matchesHistory } from './history-search.js';
import { icon } from './icons.js';
import { mcpContextPicker } from './mcp-context.js';
import { settingsPanel, permissionName } from './settings.js';

const tauri = window.__TAURI__;
const invoke = (command, args = {}) => tauri.core.invoke(command, args);
const io = {
  open: (options) => invoke('plugin:dialog|open', { options }),
  save: (options) => invoke('plugin:dialog|save', { options }),
};
const modes = {
  chat: { label: 'Model Chat', glyph: 'chat', placeholder: 'Write a message…', headline: 'What’s on your mind?' },
  agent: { label: 'Code Agent', glyph: 'code', placeholder: 'Describe a change or ask about your project…', headline: 'What are we building?' },
};

let data;
let active;
let mode = 'chat';
let sidebarVisible = true;
const turns = new Map();
const drafts = new Map();
const collapsedProjects = new Set();
const lastThreadByMode = new Map();
const colorPreference = window.matchMedia('(prefers-color-scheme: dark)');
let liveAppearance;
let lastRender;
let unreadApprovals = new Map();
let navQueue = Promise.resolve();

const current = () => data?.threads.find((thread) => thread.id === active);
const live = () => turns.get(active);
const draftOf = (id = active) => {
  if (!drafts.has(id)) drafts.set(id, { text: '', attachments: [], resources: [] });
  return drafts.get(id);
};

function toast(message, error = false) {
  const item = el('div', { class: 'toast' + (error ? ' error' : '') },
    icon(error ? 'close' : 'check', 16), el('span', {}, message),
    button('Dismiss', () => item.remove(), { glyph: 'close', onlyIcon: true, style: 'icon-button small' }));
  $('toasts').append(item);
  setTimeout(() => item.remove(), error ? 11000 : 4000);
}

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const area = el('textarea', { class: 'clipboard-buffer' }, text);
    const container = $('details').open ? $('details') : $('settings').open ? $('settings') : document.body;
    container.append(area); area.select();
    const copied = document.execCommand('copy');
    area.remove();
    if (!copied) { toast('Clipboard access failed.', true); return; }
  }
  toast('Copied to clipboard.');
}

function previewAppearance(appearance) {
  liveAppearance = appearance;
  const root = document.documentElement;
  root.dataset.theme = appearance.theme === 'system' ? colorPreference.matches ? 'dark' : 'light' : appearance.theme;
  root.dataset.density = appearance.density;
  root.dataset.font = appearance.font;
  root.dataset.motion = appearance.motion;
  root.style.setProperty('--accent', appearance.accent);
  const [r, g, b] = appearance.accent.slice(1).match(/../g).map((hex) => Number.parseInt(hex, 16) / 255);
  const linear = (channel) => channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4;
  const lightness = .2126 * linear(r) + .7152 * linear(g) + .0722 * linear(b);
  root.style.setProperty('--accent-ink', lightness > .21 ? '#101814' : '#fff');
  root.style.setProperty('--scale', appearance.font_scale + '%');
  root.style.setProperty('--content-width', appearance.width + 'px');
  root.style.setProperty('--sidebar-width', appearance.sidebar + 'px');
  root.style.setProperty('--radius', appearance.radius + 'px');
}

async function refresh() {
  data = await invoke('snapshot');
  if (!current()) active = data.active_thread ?? data.threads.at(-1)?.id;
  previewAppearance(data.appearance);
  render();
}

async function savePreferences(changes) {
  const next = { ...data, ...changes };
  await invoke('save_settings', {
    appearance: next.appearance, permissions: next.permissions,
    codexPath: next.codex_path, instructions: next.instructions,
  });
  await refresh();
}

const settings = settingsPanel({ data: () => data, invoke, io, refresh, savePreferences, previewAppearance, toast, copy, confirm });

function showDetails(title, content) {
  $('details-title').textContent = title;
  $('details-content').replaceChildren(content);
  if (!$('details').open) $('details').showModal();
}

function confirm(title, text, action, label = 'Remove') {
  const actionButton = button(label, async () => {
    actionButton.disabled = true;
    $('details').close();
    try { await action(); }
    catch (error) { toast(String(error), true); }
  }, { style: label === 'Remove' ? 'button danger' : 'button primary' });
  showDetails(title, el('div', { class: 'details-form' },
    el('p', { class: 'preserve' }, text),
    el('div', { class: 'form-actions right' },
      button('Cancel', () => $('details').close()), actionButton)));
}

function transient(operation) {
  return async (...args) => {
    try { await operation(...args); }
    catch (error) { toast(String(error), true); }
  };
}

function updateThread(thread) {
  const index = data.threads.findIndex((value) => value.id === thread.id);
  if (index >= 0) data.threads[index] = thread;
  else data.threads.push(thread);
  render();
}

function queueNavigation(operation) {
  navQueue = navQueue.then(operation).catch((error) => toast(String(error), true));
  return navQueue;
}

function activateThread(thread) {
  if (thread.project_id) collapsedProjects.delete(thread.project_id);
  active = thread.id;
  mode = thread.mode;
  lastThreadByMode.set(mode, thread.id);
  updateThread(thread);
  $('prompt').focus();
}

async function makeThread(nextMode = mode, project = nextMode === 'agent' ? current()?.project_id : null) {
  activateThread(await invoke('new_thread', { mode: nextMode, project: project ?? null }));
}

async function openThread(id) {
  await invoke('set_active', { id });
  active = id;
  mode = current().mode;
  lastThreadByMode.set(mode, id);
  render();
  scrollDown(true);
}

function conversationButton(thread, nested = false) {
  return el('button', {
    type: 'button', title: thread.title, 'aria-current': thread.id === active ? 'page' : null,
    class: 'history-item' + (nested ? ' nested' : '') + (thread.id === active ? ' current' : ''),
    onclick: () => queueNavigation(() => openThread(thread.id)),
  }, icon(thread.mode === 'agent' ? 'code' : 'chat', 15), el('span', { class: 'history-name' }, thread.title),
  unreadApprovals.has(thread.id) ? el('span', { class: 'history-alert', title: 'Approval required' }) :
    turns.has(thread.id) ? el('span', { class: 'history-running', title: 'Generating' }) : null);
}

function projectGroup(project, threads) {
  const expanded = Boolean($('history-search').value.trim()) || !collapsedProjects.has(project.id);
  const currentProject = current()?.project_id === project.id;
  const nestedId = 'project-threads-' + project.id;
  const toggle = el('button', {
    type: 'button', class: 'project-toggle', title: project.root,
    'aria-expanded': String(expanded), 'aria-controls': nestedId,
    onclick: () => {
      if (expanded) collapsedProjects.add(project.id);
      else collapsedProjects.delete(project.id);
      navigation();
    },
  }, icon('chevron', 13), icon('folder', 17), el('span', {}, project.name));
  return el('section', { class: 'project-group' + (currentProject ? ' current-project' : ''), 'aria-label': project.name },
    el('div', { class: 'project-heading' }, toggle,
      button('New thread in ' + project.name, () => queueNavigation(() => makeThread('agent', project.id)), {
        glyph: 'plus', style: 'icon-button small', onlyIcon: true,
      })),
    el('div', { id: nestedId, class: 'project-threads', hidden: !expanded },
      threads.length ? threads.map((thread) => conversationButton(thread, true)) : el('p', { class: 'muted' }, 'No threads yet.')));
}

function showModeOptions(dropdown, open) {
  dropdown.classList.toggle('open', open);
  dropdown.querySelector('.mode-current').setAttribute('aria-expanded', String(open));
  dropdown.querySelector('.mode-options').hidden = !open;
}

function modeDropdown() {
  const toggle = el('button', {
    type: 'button', class: 'mode-current', 'aria-label': 'Switch mode, current ' + modes[mode].label,
    'aria-expanded': 'false', 'aria-controls': 'mode-options',
    onclick: () => showModeOptions(dropdown, !dropdown.classList.contains('open')),
  }, icon(modes[mode].glyph, 19), el('span', {}, modes[mode].label), icon('chevron', 15));
  const entries = Object.entries(modes).map(([key, item]) => el('button', {
    type: 'button', class: 'mode-option' + (mode === key ? ' selected' : ''),
    'aria-current': mode === key ? 'page' : null,
    onclick: () => {
      showModeOptions(dropdown, false);
      queueNavigation(async () => {
        if (mode === key) return;
        const target = data.threads.find((thread) => thread.id === lastThreadByMode.get(key))
          ?? data.threads.findLast((thread) => thread.mode === key);
        if (target) await openThread(target.id);
        else await makeThread(key, null);
      });
    },
  }, icon(item.glyph, 17), el('span', {}, item.label), mode === key && icon('check', 15)));
  const dropdown = el('div', { class: 'mode-dropdown', id: 'mode-dropdown' }, toggle,
    el('div', { class: 'mode-options', id: 'mode-options', hidden: true }, entries));
  dropdown.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && dropdown.classList.contains('open')) {
      event.preventDefault();
      showModeOptions(dropdown, false);
      toggle.focus();
    }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      showModeOptions(dropdown, true);
      const index = entries.indexOf(document.activeElement);
      const direction = event.key === 'ArrowDown' ? 1 : -1;
      const next = index === -1 ? (direction === 1 ? 0 : entries.length - 1) :
        (index + entries.length + direction) % entries.length;
      entries[next].focus();
    }
  });
  return dropdown;
}

function navigation() {
  $('modes').replaceChildren(modeDropdown());
  $('history-label').textContent = mode === 'agent' ? 'Projects' : 'Conversations';
  $('new-thread').title = mode === 'agent' ? 'Add project' : 'New conversation';
  $('new-thread').setAttribute('aria-label', $('new-thread').title);

  const query = $('history-search').value;
  const projects = new Map(data.projects.map((project) => [project.id, project]));
  const visible = data.threads.filter((thread) => thread.mode === mode && matchesHistory(thread, projects.get(thread.project_id), query)).toReversed();
  const items = mode === 'chat' ? visible.map((thread) => conversationButton(thread)) : [
    ...data.projects.filter((project) => matchesHistory(null, project, query) || visible.some((thread) => thread.project_id === project.id))
      .map((project) => projectGroup(project, visible.filter((thread) => thread.project_id === project.id))),
    ...visible.some((thread) => !thread.project_id) ? [
      el('div', { class: 'unassigned-heading' }, 'No project'),
      ...visible.filter((thread) => !thread.project_id).map((thread) => conversationButton(thread)),
    ] : [],
  ];
  $('history').replaceChildren(...items);
  if (!items.length) $('history').append(el('p', { class: 'muted' }, query.trim() ? 'No matching conversations or projects.' : mode === 'agent' ? 'No projects yet.' : 'No conversations yet.'));
  $('provider-summary').textContent = data.providers.length
    ? `${data.providers.length} provider${data.providers.length === 1 ? '' : 's'} configured`
    : 'Add a provider';
}

const addProject = transient(async () => {
  const path = await io.open({ directory: true, multiple: false, title: 'Add a project folder' });
  if (!path) return;
  const thread = await invoke('open_project', { path });
  active = thread.id;
  mode = thread.mode;
  lastThreadByMode.set(mode, thread.id);
  collapsedProjects.delete(thread.project_id);
  await refresh();
  $('prompt').focus();
});

const chooseProject = transient(async () => {
  const id = active;
  async function setProject(changes) {
    const thread = await invoke('edit_thread', { id, changes });
    collapsedProjects.delete(thread.project_id);
    if ($('details').open) $('details').close();
    await refresh();
  }
  const addFolder = transient(async () => {
    const workspace = await io.open({ directory: true, multiple: false, title: 'Choose a project folder' });
    if (workspace) await setProject({ workspace });
  });
  if (!data.projects.length) {
    await addFolder();
    return;
  }
  showDetails('Choose a project', el('div', { class: 'details-form' },
    el('div', { class: 'setup-model-list' },
      data.projects.map((project) => button(project.name, transient(() => setProject({ project: project.id })), {
        glyph: 'folder', title: project.root, style: 'catalog-result',
      }))),
    el('div', { class: 'form-actions' }, button('Add project folder', addFolder, { glyph: 'plus', style: 'button primary' }))));
});

const review = transient(async () => {
  const id = active;
  const staged = el('div', { class: 'details-form' }, el('p', { class: 'muted' }, 'Reading Git status…'));
  showDetails('Project changes', staged);
  let diff;
  try { diff = await invoke('review_changes', { id }); }
  catch (error) { staged.replaceChildren(el('p', { class: 'status-error preserve' }, String(error))); return; }
  const output = el('pre', { class: 'git-diff' });
  output.replaceChildren(...diff.split('\n').map((line) => el('span', {
    class: line.startsWith('+') ? 'added' : line.startsWith('-') ? 'removed' : '',
  }, line + '\n')));
  staged.replaceChildren(output, el('div', { class: 'form-actions right' },
    button('Copy diff', () => copy(diff), { glyph: 'copy' })));
});

function worktree() {
  const id = active;
  let location = '';
  const branch = el('input', { type: 'text', required: true, placeholder: 'feature/my-change', spellcheck: 'false' });
  const path = el('span', { class: 'muted' }, 'No folder selected');
  const status = el('span', { class: 'form-status', role: 'status' });
  const submit = el('button', { type: 'submit', class: 'button primary' }, 'Create worktree');
  showDetails('New Git worktree', el('form', { class: 'details-form', onsubmit: transient(async (event) => {
    event.preventDefault();
    if (!location) { status.textContent = 'Choose a new worktree folder.'; return; }
    submit.disabled = true;
    status.textContent = 'Creating worktree…';
    try {
      updateThread(await invoke('add_worktree', { id, target: location, branch: branch.value }));
      $('details').close(); toast('Worktree ready for this conversation.');
    } catch (error) { status.textContent = String(error); }
    finally { submit.disabled = false; }
  }) }, el('p', { class: 'muted' }, 'Create a branch from the current project’s HEAD in a separate folder. This conversation will use that folder.'),
  field('Branch name', branch),
  el('div', { class: 'inline-form' }, button('Choose new folder path', transient(async () => {
    const result = await io.save({ title: 'Choose the new worktree path', defaultPath: 'opendex-worktree' });
    if (result) { location = result; path.textContent = result; }
  }), { glyph: 'folder' }), path),
  el('div', { class: 'form-actions' }, submit, status)));
}

function threadMenu(thread) {
  const id = thread.id;
  const busy = turns.has(id);
  const title = el('input', { type: 'text', value: thread.title, required: true, maxlength: 130, 'aria-label': 'Conversation title' });
  const instructions = el('textarea', { rows: 5, maxlength: 32000 }, thread.instructions);
  let scope = thread.mcp_scope?.mode ?? 'all';
  const selected = new Set(thread.mcp_scope?.servers);
  const serverList = el('div', { class: 'mcp-server-selection', hidden: scope === 'all' },
    data.servers.length
      ? data.servers.map((server) => check(server.name + (server.enabled ? '' : ' (disabled in Settings)'),
        selected.has(server.id), (value) => value ? selected.add(server.id) : selected.delete(server.id)))
      : el('p', { class: 'muted' }, 'Add an MCP server in Settings to select it here.'));
  const serverScope = options([['all', 'All enabled servers'], ['selected', 'Only selected servers']], scope, (value) => {
    scope = value;
    serverList.hidden = scope === 'all';
  }, { disabled: busy });
  const form = el('form', { class: 'details-form', onsubmit: transient(async (event) => {
    event.preventDefault();
    const mcp_scope = scope === 'all' ? { mode: 'all' } : { mode: 'selected', servers: [...selected] };
    updateThread(await invoke('edit_thread', { id, changes: { title: title.value, instructions: instructions.value, mcp_scope } }));
    $('details').close();
  }) }, field('Title', title),
  field('Thread instructions', instructions),
  field('MCP servers', serverScope),
  serverList,
  el('div', { class: 'form-actions' }, el('button', { type: 'submit', class: 'button primary', disabled: busy }, 'Save conversation'),
    button('Export Markdown', transient(async () => {
      const path = await io.save({ title: 'Export conversation', defaultPath: 'conversation.md', filters: [{ name: 'Markdown', extensions: ['md'] }] });
      if (path) { await invoke('export_thread', { id, path }); toast('Conversation exported.'); }
    }), { glyph: 'note' }),
    data.threads.some((item) => item.id === thread.parent_thread) &&
      button('Open original conversation', () => {
        $('details').close();
        queueNavigation(() => openThread(thread.parent_thread));
      }, { glyph: 'branch' }),
    thread.workspace && button('Clear project', transient(async () => {
      updateThread(await invoke('edit_thread', { id, changes: { clear_workspace: true } }));
      $('details').close();
    }), { disabled: busy }),
    button('Delete conversation', () => confirm('Delete conversation', 'This deletes the local transcript.', async () => {
      await invoke('remove_thread', { id }); drafts.delete(id);
      if (active === id) active = null;
      await refresh();
      if (current()) { mode = current().mode; render(); }
      else await makeThread(mode);
    }), { style: 'button danger quiet', disabled: busy })));
  showDetails('Conversation settings', form);
}

function toolbar(thread) {
  $('mode-title').textContent = modes[mode].label;
  $('thread-title').textContent = thread?.messages.length || thread?.title !== 'New conversation' ? thread?.title ?? '' : '';
  const root = thread?.workspace;
  $('thread-actions').replaceChildren(
    ...mode === 'agent' ? [
      button(root ? root.split(/[\\/]/).at(-1) : 'Choose project', chooseProject, {
        title: root ?? 'Choose a project folder', glyph: 'folder', style: 'button toolbar-button', disabled: Boolean(live()),
      }),
      button('Create worktree', worktree, { glyph: 'branch', onlyIcon: true, style: 'icon-button', disabled: !root || Boolean(live()) }),
      button('Review changes', review, { glyph: 'diff', onlyIcon: true, style: 'icon-button', disabled: !root }),
    ] : [],
    button('Conversation settings', () => threadMenu(thread), { glyph: 'more', onlyIcon: true, style: 'icon-button', disabled: !thread }),
  );
}

async function setSelection(thread, provider, model) {
  if (!thread || !provider) return;
  const changed = await invoke('edit_thread', { id: thread.id, changes: { selection: { provider, model: model || null } } });
  updateThread(changed);
}

function modelPicker(thread) {
  const provider = data.providers.find((item) => item.id === thread?.provider_id) ?? data.providers[0];
  const model = provider?.models.includes(thread?.model_id) ? thread.model_id : null;
  const items = data.providers.map((item) => el('option', { value: item.id }, item.name));
  $('provider-select').replaceChildren(...(items.length ? items : [el('option', { value: '' }, 'Add provider')]));
  $('provider-select').value = provider?.id ?? '';
  $('model-select').replaceChildren(
    ...model ? [] : [el('option', { value: '' }, provider?.models.length ? 'Choose a model' : 'Add models')],
    ...(provider?.models ?? []).map((id) => el('option', { value: id }, id)),
  );
  $('model-select').value = model ?? '';
  $('provider-select').disabled = Boolean(live()) || !items.length;
  $('model-select').disabled = Boolean(live()) || !provider?.models.length;
  $('permission-label').replaceChildren(...mode === 'agent' ? [
    button(permissionName(data.permissions.profile), () => settings.open('permissions'), {
      glyph: 'shield', title: 'Permission profile', style: 'permission-pill',
    }),
  ] : []);
  const empty = !provider || !provider.models.length;
  $('composer-footer').replaceChildren(
    ...empty ? [button(provider ? 'Manage models' : 'Add a provider', () => settings.open('providers', provider?.id), { style: 'text-link' })]
      : [el('span', {}, 'Enter to send'), el('span', { class: 'footer-dot' }),
        el('span', {}, 'Shift + Enter for a new line')]);
}

async function findModels(thread, provider) {
  if (!provider) { settings.open(); return; }
  if (provider.kind !== 'ollama' || provider.models.length) { settings.open('providers', provider.id); return; }
  const control = $('find-models');
  control.disabled = true;
  control.textContent = 'Looking for local models…';
  try {
    const models = await invoke('discover_models', { id: provider.id });
    if (!models.length) { toast('Ollama is running and has no installed models. Add one in Ollama, then try again.', true); settings.open('providers', provider.id); return; }
    const list = el('div', { class: 'setup-model-list' }, models.map((id) => button(id, transient(async () => {
      await invoke('add_provider_models', { id: provider.id, models: [id] });
      await refresh();
      await setSelection(thread, provider.id, id);
      $('details').close();
      $('prompt').focus();
      toast('Local model ready.');
    }), { style: 'catalog-result', glyph: 'plus' })));
    showDetails('Choose a local model', el('div', { class: 'details-form' },
      el('p', { class: 'muted' }, 'Add an installed Ollama model to your picker.'), list));
  } catch (error) {
    toast(String(error), true);
    settings.open('providers', provider.id);
  } finally {
    control.disabled = false;
    control.textContent = 'Find local models';
  }
}

function welcome(thread) {
  const config = modes[mode];
  const provider = data.providers.find((item) => item.id === thread?.provider_id) ?? data.providers[0];
  const noModels = !data.providers.some((item) => item.models.length);
  $('welcome').hidden = Boolean(thread?.messages.length || live()?.stream);
  if ($('welcome').hidden) return;
  const action = noModels
    ? el('button', { type: 'button', id: 'find-models', class: 'button primary',
      onclick: () => findModels(thread, provider) }, icon('plus', 16), provider?.kind === 'ollama' ? 'Find local models' : 'Add a model')
    : mode === 'agent' && !thread?.workspace ? button('Choose project', chooseProject, { glyph: 'folder', style: 'button primary' }) : null;
  const secondary = noModels && button('Provider settings', () => settings.open('providers', provider?.id), { glyph: 'settings' });
  const description = noModels ? 'Choose a model to start your first conversation.'
    : mode === 'agent' ? 'Ask about a project, make changes, or review your work.'
      : 'Start a conversation with a model you’ve connected.';
  $('welcome').replaceChildren(
    el('div', { class: 'welcome-mark' }, el('span', { class: 'brand-symbol', 'aria-hidden': 'true' })),
    el('h1', {}, config.headline), el('p', {}, description),
    el('div', { class: 'welcome-actions' }, action, secondary),
  );
}

async function branchConversation(thread, message, action) {
  const branch = await invoke('branch_thread', { id: thread.id, message: message.id, action });
  if (action.kind === 'edit') $('details').close();
  activateThread(branch);
  if (action.kind !== 'continue') replyToBranch(branch).catch((error) => toast(String(error), true));
}

function editMessage(thread, message) {
  const text = el('textarea', { rows: 6, maxlength: 256000,
    required: !message.attachments?.length && !message.resources?.length }, message.text);
  const submit = el('button', { type: 'submit', class: 'button primary' }, 'Send edited message');
  const status = el('p', { class: 'status-error', role: 'status' });
  const form = el('form', { class: 'details-form', onsubmit: async (event) => {
    event.preventDefault();
    submit.disabled = true;
    status.textContent = '';
    try {
      await branchConversation(thread, message, { kind: 'edit', text: text.value });
    } catch (error) {
      status.textContent = String(error);
    } finally {
      submit.disabled = false;
    }
  } }, field('Message', text),
  el('p', { class: 'muted' }, 'The edited message and its attachments will be sent in a new branch.'),
  status,
  el('div', { class: 'form-actions right' }, button('Cancel', () => $('details').close()), submit));
  showDetails('Edit message', form);
  text.focus();
}

function messageActions(thread, message, lastVisible) {
  const user = message.role === 'user';
  const canReply = user && thread.parent_thread && thread.messages.at(-1)?.id === message.id;
  const action = (label, glyph, handler) => button(label, handler, { glyph, onlyIcon: true, style: 'icon-button small' });
  const branch = () => queueNavigation(() => branchConversation(thread, message, { kind: 'continue' }));
  if (message.image_generation) {
    return el('div', { class: 'message-actions' },
      user && action('Generate image again', 'refresh', () => imagePrompt(thread, message.text, message.image_generation)),
      action('Branch from here', 'branch', branch));
  }
  return el('div', { class: 'message-actions' },
    user && thread.mode === 'chat' && action('Edit message', 'edit', () => editMessage(thread, message)),
    !user && action('Copy response', 'copy', () => copy(message.text)),
    thread.mode === 'chat' && (!user || lastVisible) && (canReply
      ? action('Generate reply', 'refresh', transient(() => replyToBranch(thread)))
      : action('Retry reply', 'refresh', () => queueNavigation(() => branchConversation(thread, message, { kind: 'retry' })))),
    action('Branch from here', 'branch', branch),
  );
}

function webResult(message) {
  if (!['web_search', 'web_fetch'].includes(message.tool_name)) return null;
  let result;
  try { result = JSON.parse(message.text); }
  catch { return null; }
  if (!Array.isArray(result?.sources)) return null;

  const summary = message.tool_name === 'web_search'
    ? `Web search · ${result.sources.length} ${result.sources.length === 1 ? 'source' : 'sources'}`
    : 'Read web page';
  return el('details', { class: 'tool-result web-result' },
    el('summary', {}, icon('search', 15), summary),
    el('div', { class: 'web-sources' },
      result.query && el('p', { class: 'muted' }, result.query),
      result.sources.length === 0 && el('p', { class: 'muted' }, 'No sources returned.'),
      result.sources.map((source) => el('article', { class: 'web-source' },
        button(source.title, transient(() => invoke('open_web_source', { url: source.url })),
          { glyph: 'arrow', style: 'text-link', title: source.url }),
        el('div', { class: 'muted source-url' }, source.url),
        source.content && el('p', {}, source.content)))));
}

function messageCard(thread, message, lastVisible) {
  const user = message.role === 'user';
  if (message.role === 'tool') {
    return webResult(message) ?? el('details', { class: 'tool-result' }, el('summary', {}, icon('terminal', 15), message.tool_label ?? message.tool_name ?? 'Tool result'),
      el('pre', {}, message.text));
  }
  if (!message.text && !message.attachments?.length && !message.resources?.length) return null;
  const body = user ? el('div', { class: 'user-text' }, message.text) : markdown(message.text, copy);
  const imageOutput = !user && message.image_generation;
  const meta = user ? null : el('div', { class: 'assistant-meta' },
    el('span', { class: 'brand-symbol tiny', 'aria-hidden': 'true' }),
    el('span', {}, message.model ?? 'Assistant'),
    message.incomplete && el('span', { class: 'muted' }, '· Partial response'));
  return el('article', { class: 'message ' + (user ? 'user' : 'assistant') }, meta,
    el('div', { class: 'message-content' }, body,
      message.attachments?.length > 0 && el('div', { class: 'message-attachments' },
        message.attachments.map((file) => el('div', {},
          !imageOutput && el('div', { class: 'chip file-chip' }, icon('note', 15), file.name),
          file.media_type?.startsWith('image/') && el('img', {
            class: 'preview-image' + (imageOutput ? ' generated-image' : ''), src: 'data:' + file.media_type + ';base64,' + file.text,
            alt: imageOutput ? 'Generated image' : file.name, loading: 'lazy',
          }),
          imageOutput && button('Save image', transient(() => saveImage(thread, message, file)),
            { glyph: 'download', style: 'button save-image' })))),
      message.resources?.map((resource) => el('details', { class: 'tool-result message-resource' },
        el('summary', {}, icon('plug', 15), resource.name, el('small', {}, ' · ' + resource.source)),
        el('pre', {}, resource.text)))),
    messageActions(thread, message, lastVisible));
}

async function saveImage(thread, message, file) {
  const extension = file.name.split('.').at(-1);
  const path = await io.save({ title: 'Save generated image', defaultPath: file.name,
    filters: [{ name: 'Image', extensions: [extension] }] });
  if (!path) return;
  await invoke('save_generated_image', { id: thread.id, message: message.id, path });
  toast('Image saved.');
}

function imagePrompt(thread, text, preferred = data.image_generation) {
  if (!thread || turns.has(thread.id)) return;
  const connections = data.providers.filter((provider) => ['open_ai', 'open_ai_compatible'].includes(provider.kind));
  if (!connections.length) {
    showDetails('Generate image', el('div', { class: 'details-form' },
      el('p', { class: 'muted' }, 'Add an OpenAI API or OpenAI-compatible connection with an image generation endpoint.'),
      button('Open provider settings', () => { $('details').close(); settings.open('providers'); }, { glyph: 'settings' })));
    return;
  }
  const initial = connections.find((item) => item.id === preferred?.provider)
    ?? connections.find((item) => item.id === thread.provider_id) ?? connections[0];
  const prompt = el('textarea', { rows: 6, required: true, maxlength: 32000 }, text);
  const model = el('input', { type: 'text', required: true, maxlength: 300, spellcheck: 'false', value: preferred?.model ?? '' });
  const size = el('input', { type: 'text', value: preferred?.size ?? '', placeholder: 'Provider default', maxlength: 20 });
  const quality = options([['', 'Provider default'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High'],
    ['xhigh', 'Extra high'], ['max', 'Maximum'], ['standard', 'Standard'], ['hd', 'HD']], preferred?.quality === 'auto' ? '' : preferred?.quality);
  let requestBase64 = preferred?.request_base64 ?? false;
  const compatibility = el('div', { hidden: initial.kind !== 'open_ai_compatible' },
    check('Request Base64 output', requestBase64, (value) => { requestBase64 = value; }));
  const connectionAddress = el('span', { class: 'field-note' }, initial.endpoint);
  const provider = options(connections.map((item) => [item.id, item.name]), initial.id, (id) => {
    const connection = connections.find((item) => item.id === id);
    compatibility.hidden = connection.kind !== 'open_ai_compatible';
    connectionAddress.textContent = connection.endpoint;
  });
  const submit = el('button', { type: 'submit', class: 'button primary' }, icon('image', 16), 'Generate');
  const form = el('form', { class: 'details-form', onsubmit: transient(async (event) => {
    event.preventDefault();
    if (turns.has(thread.id) || data.threads.every((item) => item.id !== thread.id)) return;
    const config = { provider: provider.value, model: model.value, size: size.value || null,
      quality: quality.value || null, request_base64: !compatibility.hidden && requestBase64 };
    const draft = draftOf(thread.id);
    const pending = { text: prompt.value, config };
    draft.pendingImage = pending;
    $('details').close();
    const saved = await generateReply(thread, 'generate_image', { prompt: { thread: thread.id, ...pending } }, undefined, 'Generating image…');
    if (saved && draft.pendingImage === pending) delete draft.pendingImage;
  }) },
  field('Image prompt', prompt),
  el('div', { class: 'form-grid' },
    el('div', {}, field('Image provider', provider), connectionAddress),
    field('Image model ID', model),
    field('Size', size, 'Enter dimensions such as 1024x1024.'),
    field('Quality', quality)),
  compatibility,
  el('div', { class: 'form-actions right' }, button('Cancel', () => $('details').close()), submit));
  showDetails('Generate image', form);
  prompt.focus();
}

function nearBottom() {
  const stage = $('stage');
  return stage.scrollHeight - stage.scrollTop - stage.clientHeight < 110;
}

function scrollDown(force = false) {
  if (force || nearBottom()) requestAnimationFrame(() => { $('stage').scrollTop = $('stage').scrollHeight; });
}

function timeline(thread, forced = false) {
  const shouldScroll = forced || nearBottom() || lastRender !== thread?.id;
  lastRender = thread?.id;
  const turn = live();
  const messages = thread?.messages ?? [];
  const lastVisible = messages.findLast((message) => message.role !== 'tool' &&
    (message.text || message.attachments?.length || message.resources?.length));
  const items = messages.map((message) => messageCard(thread, message, lastVisible === message)).filter(Boolean);
  if (turn?.stream) {
    items.push(el('article', { class: 'message assistant' },
      el('div', { class: 'assistant-meta' }, el('span', { class: 'brand-symbol tiny' }), thread.model_id ?? 'Assistant'),
      el('div', { class: 'stream-text', id: 'stream-text' }, turn.stream)));
  }
  $('timeline').replaceChildren(...items);
  if (shouldScroll) scrollDown(true);
}

function renderApproval(approval) {
  if (!approval) return null;
  return el('div', { class: 'approval-card' },
    el('div', { class: 'approval-title' }, icon('shield', 18),
      el('strong', {}, `Allow ${approval.tool}?`), el('span', { class: 'muted' }, approval.source)),
    el('pre', {}, JSON.stringify(approval.arguments, null, 2)),
    el('div', { class: 'form-actions right' },
      button('Deny', transient(() => answerApproval(approval.id, false))),
      button('Allow once', transient(() => answerApproval(approval.id, true)), { style: 'button primary' })));
}

async function answerApproval(id, allow) {
  await invoke('resolve_approval', { id, allow });
  unreadApprovals.delete(active);
  render();
}

function status() {
  const turn = live();
  const node = $('run-status');
  node.replaceChildren();
  if (turn?.approval) node.append(renderApproval(turn.approval));
  else if (turn) node.append(el('div', { class: 'live-status' }, el('span', { class: 'pulse' }),
    turn.notice || (turn.stream ? 'Generating' : 'Waiting for the model')));
  else if (drafts.get(active)?.error) node.append(el('p', { class: 'status-error' }, drafts.get(active).error));
  else if (drafts.get(active)?.notice) node.append(el('p', { class: 'live-status' }, drafts.get(active).notice));
}

function renderAttachments() {
  const draft = draftOf();
  const items = draft.attachments.map((file) => el('div', { class: 'chip file-chip' },
    icon('note', 15), file.name,
    button('Remove ' + file.name, () => {
      draft.attachments = draft.attachments.filter((item) => item !== file);
      renderAttachments();
    }, { glyph: 'close', onlyIcon: true, style: 'icon-button small', disabled: draft.loadingFiles })));
  for (const resource of draft.resources) {
    items.push(el('div', { class: 'chip file-chip', title: resource.source + ' · ' + resource.uri },
      icon('plug', 15), resource.name,
      button('Remove ' + resource.name, () => {
        draft.resources = draft.resources.filter((item) => item !== resource);
        renderAttachments();
      }, { glyph: 'close', onlyIcon: true, style: 'icon-button small' })));
  }
  $('attachment-row').hidden = items.length === 0;
  $('attachment-row').replaceChildren(...items);
}

async function addAttachments(id, paths) {
  if (!id || !paths.length || turns.has(id)) return;
  const draft = draftOf(id);
  if (draft.loadingFiles) return;
  draft.loadingFiles = true;
  if (id === active) composer(current());
  try {
    const all = [...new Set([...draft.attachments.map((file) => file.path), ...paths])];
    draft.attachments = await invoke('inspect_attachments', { paths: all });
  } finally {
    draft.loadingFiles = false;
    if (id === active) composer(current());
  }
}

function handleAttachmentDrop({ payload }) {
  const composer = $('composer');
  const rect = composer.getBoundingClientRect();
  const { position, type } = payload;
  // Wry uses logical coordinates on Cocoa and GTK, and Win32 client pixels on Windows.
  const scale = navigator.platform.startsWith('Win') ? window.devicePixelRatio : 1;
  const x = position?.x / scale;
  const y = position?.y / scale;
  const inside = Boolean(position && x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom);
  const allowed = inside && current() && !live() && !draftOf().loadingFiles && !$('settings').open && !$('details').open;
  composer.classList.toggle('drag-over', Boolean(allowed && (type === 'enter' || type === 'over')));
  if (allowed && type === 'drop') addAttachments(active, payload.paths).catch((error) => toast(String(error), true));
}

function openMcpContext() {
  const thread = current();
  if (!thread || live()) return;
  const policy = data.permissions;
  const allowed = policy.profile !== 'read_only' && !(policy.profile === 'custom' && policy.mcp === 'deny');
  const close = () => $('details').close();
  const selected = thread.mcp_scope?.servers;
  const enabled = data.servers.filter((server) => server.enabled);
  mcpContextPicker({
    servers: enabled.filter((server) => !selected || selected.includes(server.id)),
    manageScope: selected && enabled.length > 0 ? () => threadMenu(thread) : null,
    allowed, invoke, thread: thread.id, show: showDetails, close,
    settings: (tab) => settings.open(tab),
    addResource: (resource) => {
      const draft = draftOf(thread.id);
      const resources = draft.resources.filter((item) => item.server_id !== resource.server_id || item.uri !== resource.uri);
      if (resources.length >= 8) { toast('Attach up to eight MCP resources per message.', true); return; }
      draft.resources = [...resources, resource];
      close();
      if (active === thread.id) { renderAttachments(); $('prompt').focus(); }
    },
    insertPrompt: (messages) => {
      const draft = draftOf(thread.id);
      const text = messages.length === 1 && messages[0].role === 'user'
        ? messages[0].text
        : messages.map(({ role, text }) => (role === 'assistant' ? 'Assistant example' : 'User message') + '\n' + text).join('\n\n');
      draft.text = [draft.text, text].filter(Boolean).join('\n\n');
      close();
      if (active === thread.id) {
        $('prompt').value = draft.text;
        $('prompt').focus();
        $('prompt').setSelectionRange(draft.text.length, draft.text.length);
      }
    },
  });
}

function webDenied() {
  const policy = data.permissions;
  return policy.profile === 'read_only' || (policy.profile === 'custom' && policy.web === 'deny');
}

function composer(thread) {
  const busy = Boolean(live());
  const draft = draftOf();
  if ($('prompt').dataset.thread !== thread?.id) {
    $('prompt').value = draft.text;
    $('prompt').dataset.thread = thread?.id ?? '';
  }
  $('prompt').placeholder = modes[mode].placeholder;
  $('prompt').disabled = busy || !thread;
  $('attach').disabled = busy || !thread || draft.loadingFiles;
  $('mcp-context').disabled = busy || !thread;
  $('generate-image').disabled = busy || !thread;
  const webSearch = $('web-search');
  webSearch.disabled = busy || !thread;
  webSearch.setAttribute('aria-pressed', String(Boolean(thread?.web_enabled && data.web_search && !webDenied())));
  $('send').replaceChildren(icon(busy ? 'stop' : 'send', 21));
  $('send').title = busy ? 'Stop response' : 'Send message';
  $('send').setAttribute('aria-label', $('send').title);
  $('send').disabled = !thread || draft.loadingFiles;
  renderAttachments();
  modelPicker(thread);
  status();
}

function render() {
  if (!data) return;
  const thread = current();
  navigation();
  toolbar(thread);
  welcome(thread);
  timeline(thread);
  composer(thread);
}

function hasSelectedModel(thread) {
  const provider = data.providers.find((item) => item.id === thread.provider_id);
  if (provider?.models.includes(thread.model_id)) return true;
  toast('Choose a provider and model before sending.', true);
  if (thread.id === active) {
    if (!provider?.models.length) settings.open('providers', provider?.id);
    else $('model-select').focus();
  }
  return false;
}

async function generateReply(thread, command, args, restoreDraft, notice = '') {
  if (turns.has(thread.id)) return;
  const draft = draftOf(thread.id);
  draft.error = null;
  draft.notice = null;
  turns.set(thread.id, { stream: '', notice, approval: null, accepted: false });
  render();
  try {
    return await invoke(command, args);
  } catch (error) {
    draft.error = String(error);
    if (!turns.get(thread.id)?.accepted) restoreDraft?.();
    toast(draft.error, true);
  } finally {
    turns.delete(thread.id);
    unreadApprovals.delete(thread.id);
    await refresh();
    if (thread.id === active) { $('prompt').focus(); scrollDown(true); }
  }
}

async function replyToBranch(thread) {
  if (turns.has(thread.id) || !hasSelectedModel(thread)) return;
  const prompt = thread.messages.at(-1);
  if (prompt?.role !== 'user' || prompt.image_generation) return;
  await generateReply(thread, 'reply_in_branch', { id: thread.id, message: prompt.id });
}

async function send(event) {
  event?.preventDefault();
  const thread = current();
  if (!thread) return;
  if (live()) { await invoke('stop_turn', { id: thread.id }); return; }
  const draft = draftOf();
  if (draft.loadingFiles || (!draft.text.trim() && !draft.attachments.length && !draft.resources.length)) return;
  if (!hasSelectedModel(thread)) return;
  const submitted = { text: draft.text, attachments: draft.attachments, resources: draft.resources };
  draft.text = '';
  draft.attachments = [];
  draft.resources = [];
  $('prompt').value = '';
  await generateReply(thread, 'send_message', {
    prompt: { thread: thread.id, text: submitted.text, attachments: submitted.attachments.map((file) => file.path), resources: submitted.resources },
  }, () => {
    Object.assign(draft, submitted);
    if (thread.id === active) $('prompt').value = draft.text;
  });
}

function onTurn({ thread: id, kind, details }) {
  const turn = turns.get(id);
  const thread = data.threads.find((value) => value.id === id);
  if (!turn || !thread) return;
  switch (kind) {
    case 'accepted':
      turn.accepted = true;
      thread.title = details.title;
      if (!thread.messages.some((message) => message.id === details.message.id)) thread.messages.push(details.message);
      if (id === active) render();
      break;
    case 'delta': {
      turn.stream += details.text;
      if (id !== active) break;
      const node = $('stream-text');
      if (!node) { welcome(thread); timeline(thread); }
      else { const scroll = nearBottom(); node.append(document.createTextNode(details.text)); if (scroll) scrollDown(true); }
      break;
    }
    case 'checkpoint':
      turn.stream = '';
      if (!thread.messages.some((message) => message.id === details.message.id)) thread.messages.push(details.message);
      if (id === active) timeline(thread);
      break;
    case 'stopped':
      draftOf(id).notice = 'Turn stopped.';
      break;
    case 'notice':
      turn.notice = details.message;
      if (id === active) status();
      break;
    case 'approval':
      turn.approval = details;
      unreadApprovals.set(id, details.id);
      navigation();
      if (id === active) { status(); scrollDown(true); }
      else toast(`Approval needed in “${thread.title}”.`);
      break;
    case 'resolved':
      turn.approval = null;
      unreadApprovals.delete(id);
      navigation();
      if (id === active) status();
      break;
    case 'tool':
      thread.messages.push({ role: 'tool', tool_label: details.name, tool_name: details.tool_name, text: details.output });
      turn.notice = details.success ? `Finished ${details.name}` : `${details.name} reported an error`;
      if (id === active) { timeline(thread); status(); }
      break;
  }
}

function bind() {
  $('settings-button').replaceChildren(icon('settings', 19), 'Settings');
  $('settings-button').addEventListener('click', () => settings.open());
  $('new-thread').replaceChildren(icon('plus', 17));
  $('new-thread').addEventListener('click', () => queueNavigation(() => mode === 'agent' ? addProject() : makeThread()));
  $('close-settings').replaceChildren(icon('close', 18));
  $('close-details').replaceChildren(icon('close', 18));
  $('close-details').addEventListener('click', () => $('details').close());
  $('mcp-context').replaceChildren(icon('plug', 17));
  $('mcp-context').addEventListener('click', openMcpContext);
  $('web-search').replaceChildren(icon('search', 17));
  $('generate-image').replaceChildren(icon('image', 17));
  $('generate-image').addEventListener('click', () => {
    const thread = current();
    if (!thread) return;
    const draft = draftOf(thread.id);
    imagePrompt(thread, draft.pendingImage?.text ?? draft.text, draft.pendingImage?.config);
  });
  $('web-search').addEventListener('click', transient(async () => {
    const thread = current();
    if (!thread || live()) return;
    if (!data.web_search) { settings.open('web'); return; }
    if (webDenied()) { settings.open('permissions'); return; }
    const updated = await invoke('edit_thread', { id: thread.id, changes: { web_enabled: !thread.web_enabled } });
    updateThread(updated);
    toast(updated.web_enabled ? 'Web search is on for this conversation.' : 'Web search is off for this conversation.');
  }));
  $('attach').replaceChildren(icon('attach', 19));
  $('attach').addEventListener('click', transient(async () => {
    const id = active;
    const paths = await io.open({ multiple: true, title: 'Attach files' });
    if (paths) await addAttachments(id, Array.isArray(paths) ? paths : [paths]);
  }));
  $('toggle-sidebar').replaceChildren(icon('panel', 19));
  $('toggle-sidebar').addEventListener('click', () => {
    sidebarVisible = !sidebarVisible;
    $('app-frame').classList.toggle('sidebar-hidden', !sidebarVisible);
    $('toggle-sidebar').setAttribute('aria-expanded', String(sidebarVisible));
  });
  $('provider-select').addEventListener('change', transient(async (event) => {
    const provider = data.providers.find((item) => item.id === event.target.value);
    await setSelection(current(), provider?.id, provider?.models[0]);
  }));
  $('model-select').addEventListener('change', transient((event) => setSelection(current(), $('provider-select').value, event.target.value)));
  $('prompt').addEventListener('input', (event) => {
    const draft = draftOf();
    draft.text = event.target.value;
    if (draft.error || draft.notice) { draft.error = null; draft.notice = null; status(); }
  });
  $('history-search').addEventListener('input', navigation);
  $('history-search').addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.target.value = '';
      navigation();
    }
  });
  $('prompt').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault(); send().catch((error) => toast(String(error), true));
    }
  });
  $('composer').addEventListener('submit', transient(send));
  colorPreference.addEventListener('change', () => { if (liveAppearance) previewAppearance(liveAppearance); });
  document.addEventListener('click', (event) => {
    const dropdown = $('mode-dropdown');
    if (dropdown?.classList.contains('open') && !dropdown.contains(event.target)) showModeOptions(dropdown, false);
  });
  document.addEventListener('keydown', (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key === ',') {
      event.preventDefault(); settings.open();
    }
    if ((event.metaKey || event.ctrlKey) && event.key === 'n' && !$('settings').open && !$('details').open) {
      event.preventDefault(); queueNavigation(() => makeThread());
    }
  });
}

async function boot() {
  if (!tauri) throw new Error('Launch OpenDex as a desktop app to connect the Rust backend.');
  bind();
  await tauri.event.listen('opendex-turn', ({ payload }) => onTurn(payload));
  await tauri.event.listen('opendex-connection', ({ payload }) => settings.accountConnection(payload));
  await tauri.webview.getCurrentWebview().onDragDropEvent(handleAttachmentDrop);
  await refresh();
  if (current()) {
    mode = current().mode;
    lastThreadByMode.set(mode, current().id);
    render();
  } else await makeThread();
}

boot().catch((error) => {
  $('provider-summary').textContent = 'OpenDex could not connect';
  toast(String(error), true);
});
