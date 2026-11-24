import { button, el, field } from './dom.js';

const filters = [{ name: 'Encrypted settings backup', extensions: ['age'] }];

function passphrase(autocomplete) {
  return el('input', { type: 'password', required: true, autocomplete });
}

function submit(text) {
  return el('button', { type: 'submit', class: 'button primary' }, text);
}

function exportForm(api, section, run) {
  const password = passphrase('new-password');
  const repeated = passphrase('new-password');
  const save = submit('Export encrypted backup');
  const status = el('div', { class: 'form-status', role: 'status' });
  const form = el('form', { class: 'form-grid', onsubmit: (event) => {
    event.preventDefault();
    run(save, status, async () => {
      if (Array.from(password.value).length < 12) throw 'Use at least 12 characters.';
      if (password.value !== repeated.value) throw 'The passphrases differ.';
      const pass = password.value;
      const path = await api.io.save({ title: 'Save encrypted OpenDex backup', defaultPath: 'OpenDex-settings.age', filters });
      if (!path) { status.textContent = ''; return; }
      password.value = repeated.value = '';
      await api.invoke('export_settings_backup', { path, password: pass });
      status.textContent = 'Encrypted backup saved.';
    });
  } },
  field('New backup passphrase', password, 'Use at least 12 characters. You’ll need this passphrase to restore the backup.'),
  field('Confirm passphrase', repeated),
  el('div', { class: 'form-actions wide' }, save, status));
  return section('Export settings', 'Includes provider and MCP keys, web search, project links, permissions, appearance, and global instructions.', form);
}

function restoreForm(api, section, run, permissionName, onRestored) {
  let path;
  let revision = 0;
  const password = passphrase('current-password');
  const inspect = submit('Preview backup');
  const status = el('div', { class: 'form-status', role: 'status' });
  const filename = el('span', { class: 'field-note' }, 'No backup selected');
  const preview = el('div');
  const clearPreview = () => { revision += 1; preview.replaceChildren(); };
  const pickFile = button('Choose backup file', () => run(pickFile, status, async () => {
    const selected = await api.io.open({ title: 'Open encrypted OpenDex backup', filters });
    if (selected) {
      path = selected;
      filename.textContent = path.split(/[/\\]/).at(-1);
      password.value = '';
      clearPreview();
    }
    status.textContent = '';
  }), { glyph: 'folder' });
  password.addEventListener('input', clearPreview);
  const form = el('form', { class: 'form-grid', onsubmit: (event) => {
    event.preventDefault();
    const selected = path;
    const pass = password.value;
    clearPreview();
    const serial = revision;
    run(inspect, status, async () => {
      if (!selected) throw 'Choose an encrypted settings backup.';
      const info = await api.invoke('preview_settings_backup', { path: selected, password: pass });
      if (serial !== revision || !preview.isConnected) return;
      status.textContent = '';
      renderPreview(info);
    });
  } },
  el('div', { class: 'form-actions wide' }, pickFile, filename),
  field('Backup passphrase', password),
  el('div', { class: 'form-actions wide' }, inspect, status));
  return section('Restore settings', '', form, preview);

  function renderPreview(info) {
    const remap = {};
    const result = el('div', { class: 'form-status', role: 'status' });
    const apply = button('Restore settings', () => run(apply, result, async () => {
      const restored = await api.invoke('restore_settings_backup', {
        path, password: password.value, checksum: info.checksum, projectFolders: remap,
      });
      password.value = '';
      clearPreview();
      await onRestored();
      status.textContent = 'Settings restored.';
      if (restored.obsolete_keys_remaining) api.toast('Settings restored. Some previous keys could not be removed from the system credential store.', true);
      else api.toast('Settings restored.');
    }), { glyph: 'check', style: 'button primary' });
    const showNames = (title, values) => values.length > 0 && el('div', {},
      el('strong', {}, title),
      el('ul', {}, values.map((name) => el('li', {}, name))));
    preview.replaceChildren(el('div', { class: 'backup-preview' },
      el('h4', {}, 'Ready to restore'),
      el('p', { class: 'muted' }, 'Restoring replaces saved providers, MCP configuration, web search, permissions, appearance, and global instructions. Your current conversations and project links are kept.'),
      info.has_chatgpt_connection && el('p', { class: 'muted' }, 'Sign in again on a new machine under Providers → ChatGPT Account.'),
      showNames('Providers', info.providers),
      showNames('MCP servers', info.mcp_servers),
      info.web_search && el('p', {}, 'Web search ', el('strong', {}, info.web_search)),
      showNames('Enabled local MCP commands', info.local_servers),
      el('p', {}, 'Permission profile ', el('strong', {}, permissionName(info.permission_profile))),
      info.codex_path && el('p', {}, 'Codex executable ', el('code', {}, info.codex_path)),
      info.projects.length > 0 && el('div', { class: 'backup-projects' },
        el('strong', {}, 'Project folders'),
        info.projects.map((project) => {
          const location = el('span', { class: 'field-note' }, project.path);
          const folder = button('Choose folder', () => run(folder, result, async () => {
            const selected = await api.io.open({ title: 'Choose a folder for ' + project.name, directory: true });
            if (selected) { remap[project.id] = selected; location.textContent = selected; }
            result.textContent = '';
          }), { glyph: 'folder' });
          if (!project.available) location.append(' (unavailable on this computer)');
          return el('div', { class: 'backup-project' },
            el('div', {}, el('strong', {}, project.name), location), folder);
        })),
      el('div', { class: 'form-actions' }, apply, button('Cancel', clearPreview), result)));
  }
}

export function backupForms(api, section, run, permissionName, onRestored) {
  return [exportForm(api, section, run), restoreForm(api, section, run, permissionName, onRestored)];
}
