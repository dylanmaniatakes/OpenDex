import assert from 'node:assert/strict';
import test from 'node:test';
import { filterComposerChoices } from '../ui/model-filter.js';

const providers = [
  { id: 'ollama', name: 'Ollama Local', models: ['llama-1', 'mistral-1'] },
  { id: 'gemini', name: 'Gemini', models: ['gemini-2-flash'] },
  { id: 'claude', name: 'Claude', models: ['claude-user'] },
];

test('returns all providers and models before a query', () => {
  const result = filterComposerChoices(providers, {
    selectedProviderId: 'ollama',
    selectedModelId: 'mistral-1',
    query: '',
  });
  assert.deepEqual(result.providers, providers);
  assert.equal(result.provider?.id, 'ollama');
  assert.deepEqual(result.models, ['llama-1', 'mistral-1']);
  assert.equal(result.selectedModel, 'mistral-1');
});

test('matches provider names and models case-insensitively', () => {
  let result = filterComposerChoices(providers, {
    selectedProviderId: 'ollama',
    selectedModelId: 'mistral-1',
    query: 'mistral',
  });
  assert.deepEqual(result.providers, [providers[0]]);
  assert.equal(result.provider?.id, 'ollama');
  assert.deepEqual(result.models, ['mistral-1']);
  assert.equal(result.selectedModel, 'mistral-1');

  result = filterComposerChoices(providers, {
    selectedProviderId: 'ollama',
    selectedModelId: 'mistral-1',
    query: 'mistral',
  });
  assert.deepEqual(result.providers, [providers[0]]);
  assert.deepEqual(result.models, ['mistral-1']);
  assert.equal(result.selectedModel, 'mistral-1');
});

test('reports no matching provider or model without selecting one', () => {
  assert.deepEqual(filterComposerChoices(providers, { query: 'nope' }).providers, []);
  assert.equal(filterComposerChoices(providers, { query: 'nope' }).provider, undefined);
});
