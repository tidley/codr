import fs from 'node:fs/promises';
import { constants } from 'node:fs';
import net from 'node:net';

export async function readPrivateJSON(file, open = fs.open) {
  // Open the checked inode, not a separately checked pathname that could race.
  // Nonblocking open lets fstat reject a FIFO even when it has no writer.
  // Regular-file reads retain their normal descriptor-based semantics.
  const handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o077)
        || stat.size > 4 * 1024 * 1024) throw Error('Private room file required');
    // A same-inode writer can grow the file after stat. Bound the actual read,
    // including an overflow sentinel, instead of trusting the earlier size.
    const limit = 4 * 1024 * 1024;
    const buffer = Buffer.alloc(limit + 1);
    let used = 0;
    while (used < buffer.length) {
      const { bytesRead } = await handle.read(buffer, used, buffer.length - used, used);
      if (!bytesRead) break;
      used += bytesRead;
    }
    if (used > limit) throw Error('Private room file too large');
    return JSON.parse(buffer.subarray(0, used).toString('utf8'));
  } finally { await handle.close(); }
}

export function porterRequest(socket, request, timeout = 30000) {
  return new Promise((resolve, reject) => {
    const frame = JSON.stringify(request) + '\n';
    if (Buffer.byteLength(frame) > 1024 * 1024) return reject(Error('Room request too large'));
    const stream = net.createConnection(socket);
    stream.setEncoding('utf8');
    stream.setTimeout(timeout);
    let buffer = '', settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      stream.destroy();
      if (error) reject(error); else resolve(value);
    };
    // Socket timeout measures inactivity; an absolute timer also bounds a peer
    // that keeps sending partial frames. Both terminate without retrying.
    const deadline = setTimeout(() => finish(Error('Room preparation deadline exceeded; do not resend automatically')), timeout);
    stream.on('connect', () => stream.write(frame));
    stream.on('error', () => finish(Error('Room transport unavailable; no automatic retry')));
    stream.on('timeout', () => finish(Error('Room preparation uncertain; do not resend automatically')));
    stream.on('end', () => finish(Error('Room preparation unconfirmed; do not resend automatically')));
    stream.on('data', bytes => {
      buffer += bytes;
      if (Buffer.byteLength(buffer) > 1024 * 1024) return finish(Error('Room response too large'));
      if (!buffer.includes('\n')) return;
      try {
        const response = JSON.parse(buffer.split('\n')[0]);
        if (response.version !== 1 || response.id !== request.id || response.error || !response.result)
          return finish(Error('Room rejected input; no native fallback'));
        finish(null, response.result);
      } catch { finish(Error('Invalid room response; no native fallback')); }
    });
  });
}
