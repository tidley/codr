import { createHash } from 'node:crypto';

const digest = text => createHash('sha256').update(text).digest('hex');

// This callback runs BEFORE OpenCode admits user input. Errors propagate to
// admission; there is deliberately no native fallback or automatic send retry.
export async function preparePrompt(event, binding, { readState, request } = {}) {
  if (event.sessionID !== binding.sessionID) return;
  const { prompt, messageID } = event;
  if (!/^msg_[A-Za-z0-9_]{1,124}$/.test(messageID ?? '') || typeof prompt?.text !== 'string'
      || !prompt.text.trim() || Buffer.byteLength(prompt.text) > 30000
      || prompt.text.trimStart().startsWith('/')
      || ['files', 'agents', 'skills'].some(k => prompt[k]?.length)) {
    throw Error('Shared room accepts complete text only; input was not submitted');
  }
  const state = await readState();
  const mapping = state.mappings?.[binding.key];
  if (!mapping?.room || mapping.room_quarantined
      || mapping.group_id_hex !== binding.group
      || mapping.agent_record?.terminal_id !== binding.terminal
      || mapping.agent_record?.agent_session?.value !== binding.sessionID) {
    throw Error('Shared room is held or its binding changed; input was not submitted');
  }
  if (messageID.startsWith('msg_codr_')) {
    // A room/CLI admission already owns the bridge event loop and native lock.
    // Verify its durable ID AND text rather than reentering that same worker.
    // A prefix or caller-provided metadata alone is never bypass authority.
    const expected = mapping.room_input_fingerprints?.[messageID];
    if (expected !== digest(prompt.text.replaceAll('\r\n', '\n').trim())
        || !Object.values(mapping.shared_admissions ?? {}).includes(messageID)) {
      throw Error('Unverified Codr native admission; input was not submitted');
    }
    return;
  }
  const hex = digest(`${event.sessionID}\0${messageID}`).slice(0, 32);
  const requestID = [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join('-');
  const result = await request({ version: 1, id: requestID, request_id: requestID,
    method: 'room.desktop_prepare', bridge_account: binding.account, session_key: binding.key,
    terminal_id: binding.terminal, native_message_id: messageID, text: prompt.text });
  if (result?.published !== true || result.native_admission !== 'prepared'
      || result.native_message_id !== messageID || !result.message_ids_hex?.length
      || !result.message_ids_hex.every(id => /^[0-9a-f]{64}$/.test(id))) {
    throw Error('Shared room preparation unconfirmed; input was not submitted. Do not resend automatically');
  }
  // Preserve normal native ordering: the hook does not create a second prompt.
  event.delivery = 'queue';
}
