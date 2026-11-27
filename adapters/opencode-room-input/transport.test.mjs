import { test, expect } from 'bun:test';
import net from 'node:net';
import { execFileSync } from 'node:child_process';
import { constants } from 'node:fs';
import fs from 'node:fs/promises';
import path from 'node:path';
import { readPrivateJSON, porterRequest } from './transport.mjs';

async function fixture(run) {
  const root = await fs.mkdtemp(path.join(process.env.TMPDIR ?? '/tmp/opencode', 'codr-hook-'));
  try { await run(root); } finally { await fs.rm(root, { recursive: true, force: true }); }
}

test('binding/state reads require a private regular file', async () => fixture(async root => {
  const file = path.join(root, 'state.json');
  await fs.writeFile(file, '{"mappings":{}}', { mode: 0o600 });
  expect(await readPrivateJSON(file)).toEqual({ mappings: {} });
  await fs.chmod(file, 0o644);
  await expect(readPrivateJSON(file)).rejects.toThrow();
  await expect(readPrivateJSON(root)).rejects.toThrow();
}));

test('a FIFO without a writer rejects instead of blocking file open', async () => fixture(async root => {
  const file = path.join(root, 'not-state.fifo');
  execFileSync('mkfifo', [file]);
  const read = readPrivateJSON(file).then(() => false, () => true);
  let timer;
  const first = await Promise.race([read,
    new Promise(resolve => { timer = setTimeout(() => resolve('blocked'), 150); })]);
  clearTimeout(timer);
  // Release a blocking pre-fix reader before asserting, so a regression cannot
  // strand a fixture worker or the test runner after the assertion fails.
  if (first === 'blocked') {
    const writer = await fs.open(file, constants.O_WRONLY | constants.O_NONBLOCK);
    await writer.close();
    await read;
  }
  expect(first).toBe(true);
}));

test('Porter transport validates the response identity and never retries', async () => fixture(async root => {
  for (const mode of ['valid', 'wrong-id', 'error', 'closed', 'timeout']) {
    const socket = path.join(root, mode + '.sock');
    let requests = 0;
    const clients = new Set();
    const server = net.createServer(s => {
      clients.add(s); s.on('close', () => clients.delete(s));
      s.once('data', bytes => {
        requests++;
        const r = JSON.parse(bytes.toString().trim());
        if (mode === 'closed') return s.end();
        if (mode === 'timeout') return;
        s.end(JSON.stringify({ version: 1, id: mode === 'wrong-id' ? 'other' : r.id,
          ...(mode === 'error' ? { error: { code: 'rejected' } } : { result: { published: true } }) }) + '\n');
      });
    });
    await new Promise(resolve => server.listen(socket, resolve));
    try {
      const promise = porterRequest(socket, { id: 'test' }, 150);
      if (mode === 'valid') expect(await promise).toEqual({ published: true });
      else await expect(promise).rejects.toThrow();
      expect(requests).toBe(1);
    } finally {
      for (const s of clients) s.destroy();
      await new Promise(resolve => server.close(resolve));
    }
  }
}));

test('a trickling peer cannot extend the absolute request deadline', async () => fixture(async root => {
  const socket = path.join(root, 'trickle.sock');
  const server = net.createServer(s => {
    s.on('error', () => {});
    const tick = setInterval(() => s.write(' '), 10);
    const end = setTimeout(() => s.end(), 350);
    s.on('close', () => { clearInterval(tick); clearTimeout(end); });
  });
  await new Promise(resolve => server.listen(socket, resolve));
  const start = Date.now();
  try {
    await expect(porterRequest(socket, { id: 'deadline' }, 80)).rejects.toThrow();
    expect(Date.now() - start).toBeLessThan(250);
  } finally { await new Promise(resolve => server.close(resolve)); }
}));

test('a file growing after stat cannot exceed the read budget', async () => fixture(async root => {
  const file = path.join(root, 'growing.json');
  await fs.writeFile(file, '{}', { mode: 0o600 });
  let bytes = 0, closed = false;
  // A deterministic growing-inode fixture, independent of scheduler timing.
  const open = async () => ({
    stat: async () => ({ isFile: () => true, uid: process.getuid(), mode: 0o100600, size: 2 }),
    read: async (buffer, offset, length) => {
      bytes += length;
      buffer.fill(32, offset, offset + length);
      return { bytesRead: length };
    },
    close: async () => { closed = true; },
  });
  await expect(readPrivateJSON(file, open)).rejects.toThrow();
  expect(bytes).toBe(4 * 1024 * 1024 + 1);
  expect(closed).toBe(true);
}));
