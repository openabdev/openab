#!/usr/bin/env node
// baileys-bridge.js — Thin bridge between OAB (Rust) and WhatsApp via Baileys.
// Protocol: newline-delimited JSON over stdin/stdout.
//
// Inbound (stdout → Rust):
//   { "type": "qr",      "data": "<qr-string>" }
//   { "type": "ready",    "data": { "id": "...", "name": "..." } }
//   { "type": "message",  "data": { "from": "...", "pushName": "...", "text": "...", "messageId": "...", "isGroup": false, "participant": null } }
//   { "type": "ack",      "data": { "ack_id": "...", "success": true/false, "error": "..." } }
//   { "type": "close",    "data": { "reason": "..." } }
//
// Outbound (Rust → stdin):
//   { "action": "send",   "to": "...", "text": "...", "ack_id": "..." }

// Polyfill: Baileys 6.x requires global crypto (Node.js 19+)
if (!globalThis.crypto) {
  globalThis.crypto = require('crypto').webcrypto;
}

const { default: makeWASocket, useMultiFileAuthState, DisconnectReason, fetchLatestBaileysVersion } = require('@whiskeysockets/baileys');
const pino = require('pino');
const { Boom } = require('@hapi/boom');
const path = require('path');

// Silent logger — prevent Baileys' pino from writing to stdout (reserved for protocol)
const logger = pino({ level: 'silent' });

const SESSION_DIR = process.env.WHATSAPP_SESSION_DIR || path.join(__dirname, '.whatsapp-session');
const RECONNECT_MS = 3000;

let currentSock = null;

function emit(type, data) {
  process.stdout.write(JSON.stringify({ type, data }) + '\n');
}

async function connect(version) {
  // Clean up previous socket if any
  if (currentSock) {
    currentSock.ev.removeAllListeners();
    currentSock = null;
  }

  const { state, saveCreds } = await useMultiFileAuthState(SESSION_DIR);

  const sock = makeWASocket({
    version,
    auth: state,
    logger,
    printQRInTerminal: false,
    generateHighQualityLinkPreview: false,
  });
  currentSock = sock;

  sock.ev.on('creds.update', saveCreds);

  return new Promise((resolve) => {
    sock.ev.on('connection.update', ({ connection, lastDisconnect, qr }) => {
      if (qr) emit('qr', qr);
      if (connection === 'open') {
        const me = sock.user;
        emit('ready', { id: me?.id || '', name: me?.name || '' });
      }
      if (connection === 'close') {
        const err = lastDisconnect?.error;
        const code = (err instanceof Boom)
          ? err.output.statusCode
          : 0;
        if (err) process.stderr.write(`bridge close: ${err.message} (code=${code})\n`);
        if (code === DisconnectReason.loggedOut) {
          emit('close', { reason: 'logged_out' });
          process.exit(1);
        }
        emit('close', { reason: `disconnected_${code}` });
        resolve('reconnect');
      }
    });

    sock.ev.on('messages.upsert', ({ messages, type: upsertType }) => {
      if (upsertType !== 'notify') return;
      for (const msg of messages) {
        if (msg.key.fromMe) continue;
        // TODO: extend for media messages (images, audio, documents)
        const text = msg.message?.conversation
          || msg.message?.extendedTextMessage?.text
          || '';
        if (!text.trim()) continue;

        const isGroup = msg.key.remoteJid?.endsWith('@g.us') || false;
        emit('message', {
          from: msg.key.remoteJid,
          pushName: msg.pushName || '',
          text,
          messageId: msg.key.id,
          isGroup,
          participant: isGroup ? msg.key.participant : null,
        });
      }
    });
  });
}

// Handle commands from Rust via stdin
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', async (line) => {
  let cmd;
  try {
    cmd = JSON.parse(line);
  } catch (e) {
    process.stderr.write(`bridge error: ${e.message}\n`);
    return;
  }
  if (cmd.action === 'send') {
    const ackId = cmd.ack_id || null;
    try {
      if (!currentSock) throw new Error('not connected');
      await currentSock.sendMessage(cmd.to, { text: cmd.text });
      if (ackId) emit('ack', { ack_id: ackId, success: true });
    } catch (e) {
      process.stderr.write(`bridge send error: ${e.message}\n`);
      if (ackId) emit('ack', { ack_id: ackId, success: false, error: e.message });
    }
  }
});
rl.on('close', () => {
  // Graceful shutdown: close WhatsApp connection before exiting
  if (currentSock) {
    try { currentSock.end(); } catch (_) { /* ignore */ }
  }
  process.exit(0);
});

// Main loop with reconnect
(async () => {
  let version;
  try {
    ({ version } = await fetchLatestBaileysVersion());
  } catch (e) {
    version = [2, 3000, 1035194821]; // fallback
    process.stderr.write(`bridge: fetchLatestBaileysVersion failed, using fallback: ${e.message}\n`);
  }
  while (true) {
    try {
      const result = await connect(version);
      if (result === 'reconnect') {
        await new Promise((r) => setTimeout(r, RECONNECT_MS));
      }
    } catch (e) {
      process.stderr.write(`bridge fatal: ${e.message}\n`);
      await new Promise((r) => setTimeout(r, RECONNECT_MS));
    }
  }
})();
