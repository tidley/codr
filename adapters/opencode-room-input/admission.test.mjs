import { test, expect } from 'bun:test';
import { createHash } from 'node:crypto';
import { preparePrompt } from './admission.mjs';

const binding = { sessionID: 'ses_room', key: '["herdr:opencode","opencode","id","ses_room"]',
  terminal: 'term_room', account: 'aa'.repeat(32), group: 'bb'.repeat(16),
  statePath: '/private/room.json', socket: '/private/control.sock' };
const event = (id = 'msg_native_123', text = 'Normal desktop text') => ({
  sessionID: binding.sessionID, messageID: id, prompt: { text }, delivery: 'steer',
});
const state = () => ({ mappings: { [binding.key]: { group_id_hex: binding.group, room: {},
  agent_record: { terminal_id: binding.terminal, agent_session: { value: binding.sessionID } },
  shared_admissions: {}, room_input_fingerprints: {},
} } });
const result = e => ({ published: true, native_admission: 'prepared',
  native_message_id: e.messageID, message_ids_hex: ['cc'.repeat(32)] });

test('normal native input is prepared once without creating another native prompt', async () => {
  const e = event(), sent = [];
  const deps = { readState: async () => state(), request: async r => { sent.push(r); return result(e); } };
  await preparePrompt(e, binding, deps);
  expect(sent).toHaveLength(1);
  expect(sent[0].method).toBe('room.desktop_prepare');
  expect(sent[0].native_message_id).toBe(e.messageID);
  expect(sent[0].text).toBe(e.prompt.text);
  expect(e.delivery).toBe('queue');
  await preparePrompt(e, binding, deps);
  expect(sent[0].request_id).toBe(sent[1].request_id);
  expect(sent[0].request_id).toMatch(/^[0-9a-f-]{36}$/);
});

test('unrelated sessions do not read room state or invoke transport', async () => {
  let touched = false;
  await preparePrompt({ ...event(), sessionID: 'ses_private' }, binding, {
    readState: async () => { touched = true; throw Error('Private read'); },
    request: async () => { touched = true; throw Error('Private send'); },
  });
  expect(touched).toBe(false);
});

test('bridge-origin native admission skips reentry only with a durable matching fingerprint', async () => {
  const e = event('msg_codr_reserved'), s = state(), m = s.mappings[binding.key];
  m.shared_admissions['cc'.repeat(32)] = e.messageID;
  m.room_input_fingerprints[e.messageID] = createHash('sha256').update(e.prompt.text).digest('hex');
  let calls = 0;
  await preparePrompt(e, binding, { readState: async () => s, request: async () => { calls++; } });
  expect(calls).toBe(0);
  await expect(preparePrompt(event(e.messageID, 'Changed private input'), binding,
    { readState: async () => s })).rejects.toThrow();
  await expect(preparePrompt(event('msg_codr_forged'), binding,
    { readState: async () => s })).rejects.toThrow();
});

test('held/missing/mismatched bindings and attachments fail before publication', async () => {
  for (const failure of ['held', 'missing', 'terminal', 'group', 'files', 'skills', 'agents']) {
    const s = state(), e = event(), m = s.mappings[binding.key];
    if (failure === 'held') m.room_quarantined = 'native_input_outside_room_stream';
    if (failure === 'missing') delete s.mappings[binding.key];
    if (failure === 'terminal') m.agent_record.terminal_id = 'replacement';
    if (failure === 'group') m.group_id_hex = 'dd'.repeat(16);
    if (['files', 'skills', 'agents'].includes(failure)) e.prompt[failure] = [{}];
    let sent = false;
    await expect(preparePrompt(e, binding, { readState: async () => s,
      request: async () => { sent = true; return result(e); } })).rejects.toThrow();
    expect(sent).toBe(false);
  }
});

test('transport failures or unconfirmed receipts reject rather than fall back to native input', async () => {
  for (const reply of [null, { published: true, native_admission: 'uncertain' },
    { ...result(event()), native_message_id: 'msg_replacement' }]) {
    let calls = 0;
    await expect(preparePrompt(event(), binding, { readState: async () => state(),
      request: async () => { calls++; if (reply === null) throw Error('Transport unavailable'); return reply; }
    })).rejects.toThrow();
    expect(calls).toBe(1);
  }
});
