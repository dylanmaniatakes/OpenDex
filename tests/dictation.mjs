import assert from 'node:assert/strict';
import test from 'node:test';
import { dictationRecorder } from '../ui/dictation.js';
import { replyPlaybackProfile } from '../ui/voice.js';

test('dictation hands the transcript off for sending in the originating conversation', async () => {
  const calls = [];
  const submitted = [];
  const recorder = dictationRecorder(async (command, args) => {
    calls.push([command, args]);
    return command === 'finish_dictation' ? '  Spoken message.  ' : undefined;
  }, () => {}, async (thread, text) => {
    assert.equal(recorder.current(), null);
    await Promise.resolve();
    submitted.push([thread, text]);
  }, assert.fail);

  await recorder.toggle('conversation');
  assert.equal(recorder.current().stage, 'recording');
  assert.deepEqual(submitted, []);
  await recorder.toggle('conversation');
  assert.equal(recorder.current(), null);
  assert.deepEqual(submitted, [['conversation', 'Spoken message.']]);
  assert.deepEqual(calls.map(([command]) => command), ['start_dictation', 'finish_dictation']);
  assert.equal(calls[0][1].id, calls[1][1].id);
});

test('completed voice turns use the selected voice even when typed playback is off', () => {
  const voice = { selected: 'hermes-pocket-tts', autoplay: false };
  const reply = { role: 'assistant', text: 'Voice reply.' };
  assert.equal(replyPlaybackProfile(voice, true, reply), voice.selected);
  assert.equal(replyPlaybackProfile(voice, false, reply), null);
  assert.equal(replyPlaybackProfile({ ...voice, autoplay: true }, false, reply), voice.selected);
  assert.equal(replyPlaybackProfile({ ...voice, selected: null }, true, reply), null);
  assert.equal(replyPlaybackProfile(voice, true, { ...reply, incomplete: true }), null);
  assert.equal(replyPlaybackProfile(voice, true, { ...reply, image_generation: {} }), null);
  assert.equal(replyPlaybackProfile(voice, true, { ...reply, text: ' ' }), null);
});

test('switching conversations discards an active recording', async () => {
  const calls = [];
  const recorder = dictationRecorder(async (command) => {
    calls.push(command);
  }, () => {}, assert.fail, assert.fail);
  await recorder.toggle('original');
  recorder.keepThread('other');
  assert.equal(recorder.current(), null);
  assert.deepEqual(calls, ['start_dictation', 'cancel_dictation']);
});
