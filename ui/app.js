import { $, button, check, el, field, markdown, options } from './dom.js';
import { matchesHistory } from './history-search.js';
import { icon } from './icons.js';
import { mcpContextPicker } from './mcp-context.js';
import { settingsPanel, permissionName } from './settings.js';
import { replyPlaybackProfile, speechPlayer } from './voice.js';
import { dictationRecorder } from './dictation.js';

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
  if (!drafts.has(id)) drafts.set(id, { text: '', attachments: [], resources: [], voiceInput: false });
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

let customFontKey;
async function applyCustomFont(appearance) {
  const font = appearance.custom_font;
  const cacheKey = font ? font.file + ':' + font.name : '';
  if (!font || customFontKey === cacheKey) return;
  try {
    const dataUrl = await invoke('font_data', { file: font.file });
    const family = (font.name || 'OpenDex Custom').replaceAll('"', '');
    const face = new FontFace(family, `url(${dataUrl})`);
    await face.load();
    document.fonts.add(face);
    document.documentElement.style.setProperty('--custom-font', `"${family}"`);
    customFontKey = cacheKey;
  } catch (error) {
    toast(String(error), true);
  }
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
  if (appearance.font === 'custom') applyCustomFont(appearance);
}

async function refresh() {
  const next = await invoke('snapshot');
  if (JSON.stringify(data?.voice) !== JSON.stringify(next.voice)) speech.stop();
  data = next;
  if (!current()) active = data.active_thread ?? data.threads.at(-1)?.id;
  if (current()) mode = current().mode;
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

const speech = speechPlayer(invoke, () => {
  voiceControls();
  if (data) timeline(current());
}, toast);
const dictation = dictationRecorder(invoke, () => {
  if (data) composer(current());
}, async (id, text) => {
  const thread = data.threads.find((thread) => thread.id === id);
  if (!thread) return;
  const draft = draftOf(id);
  const separator = draft.text && !/\s$/.test(draft.text) ? ' ' : '';
  draft.text += separator + text;
  draft.voiceInput = true;
  if (id === active) {
    $('prompt').value = draft.text;
    await submitDraft(thread);
  }
}, toast);
const settings = settingsPanel({
  data: () => data, invoke, io, refresh, savePreferences, previewAppearance, toast, copy, confirm,
  stopSpeech: speech.stop,
  previewVoice: (profile) => {
    const job = speech.current();
    if (job?.profile === profile && !job.source && job.blocked) speech.toggle();
    else speech.start(profile);
  },
});

function voiceControls() {
  const control = $('voice-playback');
  const job = speech.current();
  const action = job ? (job.blocked ? 'Play voice' : 'Stop voice') : 'Voice settings';
  const label = data?.voice?.profiles.find((profile) => profile.id === data.voice.selected)?.name;
  control.replaceChildren(icon(job ? (job.blocked ? 'play' : 'stop') : 'speaker', 17));
  control.title = !job && label ? action + ' · ' + label : action;
  control.setAttribute('aria-label', action);
}

function speakReply(thread, message) {
  const job = speech.current();
  if (job?.source?.thread === thread.id && job.source.message === message.id) {
    speech.toggle();
  } else if (data.voice?.selected) {
    speech.start(data.voice.selected, { thread: thread.id, message: message.id });
  } else {
    settings.open('voice');
  }
}

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

function shareWorkspace(thread) {
  if (!data.workspace_bridge) {
    $('details').close();
    settings.open('mcp');
    return;
  }
  confirm('Share this workspace', 'Anyone with the temporary link can request tools in this project. OpenDex enforces your read settings and asks before edits and commands. The link expires after 30 minutes or when you stop sharing.', async () => {
    const url = await invoke('share_workspace', { id: thread.id });
    turns.set(thread.id, { stream: '', approval: null, accepted: true, workspaceLink: url });
    render();
    showDetails('Workspace shared', el('div', { class: 'details-form' },
      field('MCP link', el('input', { type: 'text', value: url, readonly: true })),
      el('div', { class: 'form-actions right' },
        button('Copy MCP link', () => copy(url), { style: 'button primary' }),
        button('Done', () => $('details').close()))));
  }, 'Start sharing');
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
        const target = data.threads.find((thread) => thread.mode === key && thread.id === lastThreadByMode.get(key))
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
  const codeProject = thread.mode === 'chat' ? options(
    [['', 'Ask for a folder'], ...data.projects.map((project) => [project.id, project.name]), ['off', 'Off']],
    thread.code_handoff_disabled ? 'off' : thread.code_project_id ?? '', undefined, { disabled: busy },
  ) : null;
  const form = el('form', { class: 'details-form', onsubmit: transient(async (event) => {
    event.preventDefault();
    const mcp_scope = scope === 'all' ? { mode: 'all' } : { mode: 'selected', servers: [...selected] };
    const changes = { title: title.value, instructions: instructions.value, mcp_scope };
    if (codeProject) {
      const disabled = codeProject.value === 'off';
      changes.code_project = { project: disabled ? null : codeProject.value || null, disabled };
    }
    updateThread(await invoke('edit_thread', { id, changes }));
    $('details').close();
  }) }, field('Title', title),
  field('Thread instructions', instructions),
  codeProject && field('Code Agent handoff project', codeProject, 'Use this folder when the model requests local project work.'),
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
    thread.mode === 'agent' && thread.workspace && button('Share workspace with MCP client', () => shareWorkspace(thread), { glyph: 'plug', disabled: busy }),
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
  const models = [...(provider?.models ?? [])];
  const selectedModel = models.includes(thread?.model_id) ? thread?.model_id : models[0] ?? '';
  $('provider-select').replaceChildren(...(data.providers.length ? data.providers.map((item) => el('option', { value: item.id }, item.name)) : [el('option', { value: '' }, 'Add a provider')]));
  $('provider-select').value = provider?.id ?? '';
  $('model-select').replaceChildren(
    ...models.length ? models.map((id) => el('option', { value: id }, id)) : [el('option', { value: '' }, 'Add models')],
  );
  $('model-select').value = selectedModel;
  $('provider-select').disabled = Boolean(live()) || !data.providers.length;
  $('model-select').disabled = Boolean(live()) || !models.length;
  $('permission-label').replaceChildren(...mode === 'agent' ? [
    button(permissionName(data.permissions.profile), () => settings.open('permissions'), {
      glyph: 'shield', title: 'Permission profile', style: 'permission-pill',
    }),
  ] : []);
  const empty = !provider || !provider.models.length;
  const execution = provider?.local_execution;
  const hermes = provider?.kind === 'open_ai_compatible' && /^hermes-agent$/i.test(selectedModel);
  const location = execution ? 'Local tools' : hermes && mode === 'agent'
    ? (thread?.workspace ? 'Mac and VPS tools' : 'Hermes VPS tools') : '';
  $('composer-footer').replaceChildren(
    ...empty ? [button(provider ? 'Manage models' : 'Add a provider', () => settings.open('providers', provider?.id), { style: 'text-link' })]
      : [...(location ? [el('span', { title: execution?.model ?? '' }, location), el('span', { class: 'footer-dot' })] : []),
        el('span', {}, 'Enter to send'), el('span', { class: 'footer-dot' }),
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
  const job = speech.current();
  const speaking = job?.source?.thread === thread.id && job.source.message === message.id;
  if (message.image_generation) {
    return el('div', { class: 'message-actions' },
      user && action('Generate image again', 'refresh', () => imagePrompt(thread, message.text, message.image_generation)),
      action('Branch from here', 'branch', branch));
  }
  return el('div', { class: 'message-actions' },
    user && thread.mode === 'chat' && action('Edit message', 'edit', () => editMessage(thread, message)),
    !user && action('Copy response', 'copy', () => copy(message.text)),
    !user && message.text.trim() && action(
      speaking ? (job.blocked ? 'Play voice' : 'Stop voice') : 'Read aloud',
      speaking ? (job.blocked ? 'play' : 'stop') : 'speaker',
      () => speakReply(thread, message)),
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
  const imageSource = imageOutput && (data.image_sources?.find((item) => item.id === message.provider)?.name
    ?? data.providers.find((item) => item.id === message.provider)?.name);
  const meta = user ? null : el('div', { class: 'assistant-meta' },
    el('span', { class: 'brand-symbol tiny', 'aria-hidden': 'true' }),
    el('span', {}, imageSource && imageSource !== message.model
      ? [imageSource, message.model].filter(Boolean).join(' · ') : message.model ?? 'Assistant'),
    message.incomplete && el('span', { class: 'muted' }, '· Partial response'));
  return el('article', { class: 'message ' + (user ? 'user' : 'assistant') }, meta,
    el('div', { class: 'message-content' }, body,
      message.attachments?.length > 0 && el('div', { class: 'message-attachments' },
        message.attachments.map((file) => el('div', {},
          !imageOutput && el('div', { class: 'chip file-chip' }, icon('note', 15), file.name),
          file.text && file.media_type?.startsWith('image/') && el('img', {
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
  const connections = [
    ...(data.image_sources ?? []).filter((source) => source.kind !== 'comfy_ui' || Object.keys(source.options ?? {}).length)
      .map((source) => ({ ...source, saved: true })),
    ...data.providers.filter((provider) => ['open_ai', 'open_ai_compatible'].includes(provider.kind)
      && !provider.hermes_remote_approvals),
  ];
  const manage = () => { $('details').close(); settings.open('images'); };
  if (!connections.length) {
    showDetails('Generate image', el('div', { class: 'details-form' },
      el('p', { class: 'muted' }, 'Add an image source in Settings.'),
      button('Image settings', manage, { glyph: 'settings' })));
    return;
  }
  const initial = connections.find((item) => item.id === preferred?.provider)
    ?? connections.find((item) => item.id === thread.provider_id) ?? connections[0];
  const prompt = el('textarea', { rows: 6, required: true, maxlength: 32000 }, text);
  const model = el('input', { type: 'text', required: !initial.saved, maxlength: 300, spellcheck: 'false',
    value: initial.saved ? '' : preferred?.provider === initial.id ? preferred.model : '' });
  const size = el('input', { type: 'text', value: preferred?.size ?? '', placeholder: 'Provider default', maxlength: 20 });
  const quality = options([['', 'Provider default'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High'],
    ['xhigh', 'Extra high'], ['max', 'Maximum'], ['standard', 'Standard'], ['hd', 'HD']], preferred?.quality === 'auto' ? '' : preferred?.quality);
  let requestBase64 = preferred?.request_base64 ?? false;
  const compatibility = el('div', { hidden: initial.saved || initial.kind !== 'open_ai_compatible' },
    check('Request Base64 output', requestBase64, (value) => { requestBase64 = value; }));
  const fields = el('div', { class: 'form-grid', hidden: initial.saved }, field('Image model ID', model),
    field('Size', size, 'Enter dimensions such as 1024x1024.'), field('Quality', quality));
  const connectionAddress = el('span', { class: 'field-note' },
    initial.saved ? initial.model || 'Saved source settings' : initial.endpoint);
  const provider = options(connections.map((item) => [item.id, item.name]), initial.id, (id) => {
    const connection = connections.find((item) => item.id === id);
    compatibility.hidden = connection.saved || connection.kind !== 'open_ai_compatible';
    model.required = !connection.saved;
    fields.hidden = Boolean(connection.saved);
    connectionAddress.textContent = connection.saved ? connection.model || 'Saved source settings' : connection.endpoint;
  });
  const submit = el('button', { type: 'submit', class: 'button primary' }, icon('image', 16), 'Generate');
  const form = el('form', { class: 'details-form', onsubmit: transient(async (event) => {
    event.preventDefault();
    if (turns.has(thread.id) || data.threads.every((item) => item.id !== thread.id)) return;
    const selected = connections.find((item) => item.id === provider.value);
    const config = { provider: selected.id, model: selected.saved ? selected.model || selected.name : model.value,
      size: selected.saved ? null : size.value || null,
      quality: selected.saved ? null : quality.value || null, request_base64: !compatibility.hidden && requestBase64 };
    const draft = draftOf(thread.id);
    const pending = { text: prompt.value, config };
    draft.pendingImage = pending;
    $('details').close();
    const saved = await generateReply(thread, 'generate_image', { prompt: { thread: thread.id, ...pending } }, { notice: 'Generating image…' });
    if (saved && draft.pendingImage === pending) delete draft.pendingImage;
  }) },
  field('Image prompt', prompt),
  el('div', {}, field('Image source', provider), connectionAddress),
  fields,
  compatibility,
  el('div', { class: 'form-actions right' }, button('Manage image sources', manage, { glyph: 'settings' }),
    button('Cancel', () => $('details').close()), submit));
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
      approval.always && button('Always allow', transient(() => answerApproval(approval.id, true, true))),
      button('Allow once', transient(() => answerApproval(approval.id, true)), { style: 'button primary' })));
}

async function answerApproval(id, allow, always = false) {
  try {
    await invoke('resolve_approval', { id, allow, always });
    if (always) await refresh();
  } catch (error) {
    toast(String(error), true);
  }
  unreadApprovals.delete(active);
  render();
}

function status() {
  const turn = live();
  const node = $('run-status');
  node.replaceChildren();
  const recording = dictation.current();
  if (turn?.approval) node.append(renderApproval(turn.approval));
  if (turn?.workspaceLink) {
    const id = active;
    node.append(el('div', { class: 'live-status' }, el('span', { class: 'pulse' }),
      'Workspace shared',
      button('Copy MCP link', () => copy(turn.workspaceLink), { style: 'button quiet' }),
      button('Stop sharing', transient(() => invoke('stop_turn', { id })), { style: 'button quiet' })));
    return;
  }
  if (turn?.approval) return;
  if (turn) node.append(el('div', { class: 'live-status' }, el('span', { class: 'pulse' }),
    turn.notice || (turn.stream ? 'Generating' : 'Waiting for the model')));
  else if (recording && recording.thread === active) node.append(el('div', { class: 'live-status' },
    recording.stage === 'recording' && el('span', { class: 'pulse' }),
    { starting: 'Starting microphone…', recording: 'Recording…', transcribing: 'Transcribing…' }[recording.stage],
    recording.stage === 'recording' && button('Cancel recording', dictation.cancel, { style: 'button quiet' })));
  else if (drafts.get(active)?.error) {
    const error = drafts.get(active).error;
    node.append(el('p', { class: 'status-error' }, error));
    if (error.startsWith('This is the Hermes Agent API.') || error.startsWith('Could not verify Hermes command approval support.')
      || error.startsWith('Could not confirm whether this connection runs tools on the server.')
      || error.startsWith('This endpoint does not advertise Hermes command approvals.') || error.startsWith('Local execution model unavailable.')
      || error.includes('Choose a local execution model for this connection')) {
      node.append(button('Provider settings', () => settings.open('providers', current()?.provider_id),
        { style: 'text-link' }));
    }
  }
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

const PLAN_INSTRUCTIONS = 'Before changing anything, write a short plan: what you will change, which files are involved, and how you will verify it. Then wait for my confirmation before using tools that edit files or run commands.';
const GOAL_INSTRUCTIONS = 'Start by restating the goal in one sentence and listing concrete completion criteria. Work toward them, check each criterion, and finish with what is done, what is verified, and what remains.';

async function applyInstructions(thread, instructions, label) {
  const next = thread.instructions.trim() === instructions.trim() ? '' : instructions;
  const updated = await invoke('edit_thread', { id: thread.id, changes: { instructions: next } });
  updateThread(updated);
  toast(next ? `“${label}” instructions applied to this conversation.` : `“${label}” instructions cleared.`);
}

function fillPresetMenu() {
  const host = $('preset-menu');
  const presets = (data?.presets ?? []).filter((preset) => preset.name.trim());
  host.hidden = presets.length === 0;
  host.replaceChildren(...presets.map((preset) =>
    el('button', { type: 'button', class: 'tool-entry', role: 'menuitem', title: 'Apply ' + preset.name,
      onclick: transient(() => {
        const thread = current();
        if (thread) return applyInstructions(thread, preset.instructions, preset.name);
      }) }, icon('note', 16), el('span', {}, preset.name))));
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
  $('tools-menu').disabled = !thread;
  $('plan-mode').disabled = busy || !thread;
  $('goal-mode').disabled = busy || !thread;
  const recording = dictation.current();
  const microphone = $('dictation');
  const recordingHere = recording?.thread === thread?.id && recording?.stage === 'recording';
  const micLabel = recordingHere ? 'Stop and send voice message' : 'Start voice message';
  microphone.replaceChildren(icon(recordingHere ? 'stop' : 'mic', 17));
  microphone.title = micLabel;
  microphone.setAttribute('aria-label', micLabel);
  microphone.setAttribute('aria-pressed', String(recordingHere));
  microphone.disabled = busy || !thread || (!recordingHere && (Boolean(recording) || draft.loadingFiles));
  const webSearch = $('web-search');
  const webOn = Boolean(thread?.web_enabled && data.web_search && !webDenied());
  webSearch.disabled = busy || !thread;
  webSearch.setAttribute('aria-pressed', String(webOn));
  const webState = $('web-state');
  if (webState) webState.textContent = webOn ? 'On' : 'Off';
  $('send').replaceChildren(icon(busy ? 'stop' : 'send', 21));
  $('send').title = busy ? 'Stop response' : 'Send message';
  $('send').setAttribute('aria-label', $('send').title);
  $('send').disabled = !thread || draft.loadingFiles || recording?.thread === thread.id;
  renderAttachments();
  modelPicker(thread);
  status();
}

function render() {
  if (!data) return;
  const thread = current();
  dictation.keepThread(thread?.id);
  navigation();
  toolbar(thread);
  welcome(thread);
  timeline(thread);
  composer(thread);
  voiceControls();
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

async function generateReply(thread, command, args, { restoreDraft, notice = '', voiceInput = false } = {}) {
  if (turns.has(thread.id)) return;
  const previousMessage = thread.messages.at(-1)?.id;
  const draft = draftOf(thread.id);
  draft.error = null;
  draft.notice = null;
  turns.set(thread.id, { stream: '', notice, approval: null, accepted: false });
  render();
  let completed = false;
  try {
    const result = await invoke(command, args);
    completed = true;
    return result;
  } catch (error) {
    draft.error = String(error);
    if (!turns.get(thread.id)?.accepted) restoreDraft?.();
    toast(draft.error, true);
  } finally {
    turns.delete(thread.id);
    unreadApprovals.delete(thread.id);
    await refresh();
    if (completed && data.appearance?.notifications && !document.hasFocus()) {
      invoke('notify', { title: thread.title || 'OpenDex', body: 'Your reply is ready.' }).catch(() => {});
    }
    if (thread.id === active) {
      if (!$('settings').open && !$('details').open) $('prompt').focus();
      scrollDown(true);
      const latest = current()?.messages.at(-1);
      if (completed && latest?.id !== previousMessage && !draft.notice) {
        const profile = replyPlaybackProfile(data.voice, voiceInput, latest);
        if (profile) speech.start(profile, { thread: thread.id, message: latest.id });
        else if (voiceInput && !data.voice?.selected && latest?.role === 'assistant' && latest.text.trim()) {
          toast('Choose an active voice in Settings → Voice to hear spoken replies.');
        }
      }
    }
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
  await submitDraft(thread);
}

async function submitDraft(thread) {
  if (turns.has(thread.id)) return;
  if (dictation.current()?.thread === thread.id) return;
  const draft = draftOf(thread.id);
  if (draft.loadingFiles || (!draft.text.trim() && !draft.attachments.length && !draft.resources.length)) return;
  if (!hasSelectedModel(thread)) return;
  const submitted = { text: draft.text, attachments: draft.attachments, resources: draft.resources, voiceInput: draft.voiceInput };
  draft.text = '';
  draft.attachments = [];
  draft.resources = [];
  draft.voiceInput = false;
  $('prompt').value = '';
  await generateReply(thread, 'send_message', {
    prompt: { thread: thread.id, text: submitted.text, attachments: submitted.attachments.map((file) => file.path), resources: submitted.resources },
  }, {
    voiceInput: submitted.voiceInput,
    restoreDraft: () => {
      Object.assign(draft, submitted);
      if (thread.id === active) $('prompt').value = draft.text;
    },
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
    case 'reset_stream':
      turn.stream = '';
      if (id === active) timeline(thread);
      break;
    case 'code_handoff': {
      const { thread: updated, project } = details;
      turn.stream = '';
      turn.notice = 'Continuing in Code Agent…';
      if (!data.projects.some((item) => item.id === project.id)) data.projects.push(project);
      if (lastThreadByMode.get('chat') === id) lastThreadByMode.delete('chat');
      if (id === active) {
        mode = updated.mode;
        lastThreadByMode.set(mode, id);
        collapsedProjects.delete(project.id);
      }
      updateThread(updated);
      break;
    }
    case 'stopped':
      draftOf(id).notice = 'Turn stopped.';
      break;
    case 'workspace_share_ended':
      if (!turn.workspaceLink) break;
      turns.delete(id);
      unreadApprovals.delete(id);
      render();
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
      else {
        toast(`Approval needed in “${thread.title}”.`);
        if (data.appearance?.notifications && !document.hasFocus()) {
          invoke('notify', { title: 'Approval needed', body: thread.title }).catch(() => {});
        }
      }
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
  $('mcp-context').replaceChildren(icon('plug', 17), el('span', {}, 'Add MCP context'));
  $('mcp-context').addEventListener('click', openMcpContext);
  $('web-search').replaceChildren(icon('search', 17), el('span', {}, 'Web search'), el('span', { class: 'tool-state', id: 'web-state' }, 'Off'));
  $('plan-mode').replaceChildren(icon('note', 16), el('span', {}, 'Plan first'), el('span', { class: 'tool-state' }, 'instructions'));
  $('goal-mode').replaceChildren(icon('check', 16), el('span', {}, 'Goal mode'), el('span', { class: 'tool-state' }, 'instructions'));
  $('generate-video').replaceChildren(icon('play', 16), el('span', {}, 'Generate video'), el('span', { class: 'tool-state' }, 'Soon'));
  const toolsMenu = $('tools-menu');
  toolsMenu.replaceChildren(icon('plus', 17));
  toolsMenu.addEventListener('click', () => {
    const popup = $('tools-popup');
    const open = popup.hidden;
    popup.hidden = !open;
    toolsMenu.setAttribute('aria-expanded', String(open));
    if (open) {
      fillPresetMenu();
      popup.querySelector('.tool-entry:not(:disabled)')?.focus();
    }
  });
  $('plan-mode').addEventListener('click', transient(async () => {
    const thread = current();
    if (thread) return applyInstructions(thread, PLAN_INSTRUCTIONS, 'Plan first');
  }));
  $('goal-mode').addEventListener('click', transient(async () => {
    const thread = current();
    if (thread) return applyInstructions(thread, GOAL_INSTRUCTIONS, 'Goal mode');
  }));
  $('tools-popup').addEventListener('click', (event) => {
    if (!event.target.closest('button')) return;
    $('tools-popup').hidden = true;
    toolsMenu.setAttribute('aria-expanded', 'false');
  });
  voiceControls();
  $('voice-playback').addEventListener('click', () => {
    if (speech.current()) speech.toggle();
    else settings.open('voice');
  });
  $('dictation').addEventListener('click', transient(async () => {
    const thread = current();
    if (!thread || live()) return;
    if (!dictation.current() && (data.dictation?.mode ?? 'off') === 'off') {
      settings.open('dictation');
      return;
    }
    if (!dictation.current()) {
      if (!hasSelectedModel(thread) || draftOf(thread.id).loadingFiles) return;
      speech.stop();
    }
    await dictation.toggle(thread.id);
  }));
  $('generate-image').replaceChildren(icon('image', 17), el('span', {}, 'Generate image'));
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
    if (!draft.text.trim()) draft.voiceInput = false;
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
    const popup = $('tools-popup');
    if (popup && !popup.hidden && !popup.contains(event.target) && !toolsMenu.contains(event.target)) {
      popup.hidden = true;
      toolsMenu.setAttribute('aria-expanded', 'false');
    }
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && !$('tools-popup').hidden) {
      $('tools-popup').hidden = true;
      toolsMenu.setAttribute('aria-expanded', 'false');
    }
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
