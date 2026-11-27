import { Plugin } from '@opencode/plugin';
import path from 'node:path';
import { preparePrompt } from './admission.mjs';
import { porterRequest, readPrivateJSON } from './transport.mjs';

export default Plugin.define({
  id: 'codr.room-input',
  async setup(context) {
    const file = context.options.binding_file;
    if (typeof file !== 'string' || !path.isAbsolute(file)) throw Error('Private room binding file required');
    const binding = await readPrivateJSON(file);
    for (const field of ['sessionID', 'key', 'terminal', 'account', 'group', 'statePath', 'socket']) {
      if (typeof binding[field] !== 'string' || !binding[field]) throw Error('Incomplete room input binding');
    }
    const native = JSON.parse(binding.key);
    if (native[1] !== 'opencode' || native[2] !== 'id' || native[3] !== binding.sessionID)
      throw Error('Room input native identity mismatch');
    const registration = await context.session.hook('prompt', event => preparePrompt(event, binding, {
      readState: () => readPrivateJSON(binding.statePath),
      request: request => porterRequest(binding.socket, request),
    }));
    return () => registration.dispose();
  },
});
