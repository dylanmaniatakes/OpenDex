import assert from 'node:assert/strict';
import test from 'node:test';
import { matchesHistory } from '../ui/history-search.js';

test('search matches titles, messages, and project names', () => {
  const thread = {title: 'Radio notes', messages: [{text: 'Configure the antenna'}, {text: null}]};
  assert.equal(matchesHistory(thread, null, ' RADIO '), true);
  assert.equal(matchesHistory(thread, null, 'antenna'), true);
  assert.equal(matchesHistory(thread, {name: 'Workshop'}, 'workshop'), true);
  assert.equal(matchesHistory(null, {name: 'Empty project'}, 'empty'), true);
  assert.equal(matchesHistory(thread, null, '   '), true);
  assert.equal(matchesHistory(thread, null, 'missing'), false);
});

test('search excludes credential metadata and attachments', () => {
  const thread = {title: 'Example', messages: [{text: '', attachments: [{text: 'private attachment'}]}], api_key: 'secret-key'};
  assert.equal(matchesHistory(thread, null, 'private attachment'), false);
  assert.equal(matchesHistory(thread, null, 'secret-key'), false);
});
