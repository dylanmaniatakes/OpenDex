import assert from 'node:assert/strict';
import test from 'node:test';
import { matchesHistory } from '../ui/history-search.js';

const provider = { name: 'Ollama Local', kind: 'ollama' };

test('search matches titles, messages, attachments, resources, and project names', () => {
  const thread = {
    title: 'Radio notes',
    messages: [
      {text: 'Configure the antenna', attachments: [{name: 'diagram.pdf'}]},
      {text: null, resources: [{name: 'Design notes', uri: 'notes://workshop/config'}]},
    ],
  };
  assert.equal(matchesHistory(thread, null, ' RADIO ', null), true);
  assert.equal(matchesHistory(thread, null, 'antenna', null), true);
  assert.equal(matchesHistory(thread, null, 'diagram', null), true);
  assert.equal(matchesHistory(thread, null, 'Design notes', null), true);
  assert.equal(matchesHistory(thread, null, 'workshop/config', null), true);
  assert.equal(matchesHistory(thread, {name: 'Workshop'}, 'workshop', null), true);
  assert.equal(matchesHistory(null, {name: 'Empty project'}, 'empty', null), true);
  assert.equal(matchesHistory(thread, null, '   ', null), true);
  assert.equal(matchesHistory(thread, null, 'missing', null), false);
});

test('search matches provider, model, and tool labels in threads', () => {
  const thread = {
    title: 'Fix parser',
    provider_id: 'fixture-provider',
    model_id: 'dolphin-phi:latest',
    messages: [{role: 'tool', text: 'Fixture output', tool_label: 'Read file', tool_name: 'read_file'}],
  };
  assert.equal(matchesHistory(thread, null, 'Ollama Local', provider), true);
  assert.equal(matchesHistory(thread, null, 'DOLPHIN', provider), true);
  assert.equal(matchesHistory(thread, null, 'read file', null), true);
  assert.equal(matchesHistory(thread, null, 'read_file', null), true);
  assert.equal(matchesHistory(thread, null, 'Fixture output', null), true);
  assert.equal(matchesHistory(thread, null, 'claude-sonnet', null), false);
});

test('search excludes attachment contents and credential metadata', () => {
  const thread = {
    title: 'Example',
    provider_id: 'fixture-provider',
    messages: [{text: '', attachments: [{text: 'private attachment'}]}],
    api_key: 'secret-key',
  };
  assert.equal(matchesHistory(thread, null, 'private attachment', null), false);
  assert.equal(matchesHistory(thread, null, 'secret-key', null), false);
});
