// Real CLI compatibility check; no account or model calls required.
// Run: node .github/scripts/commandcode_acp_smoke.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { createInterface } from "node:readline";

const home = await mkdtemp(join(tmpdir(), "openab-commandcode-"));
// Match OpenAB's child environment; isolate HOME so CI cannot reuse credentials.
const env = {
  HOME: home,
  PATH: dirname(process.execPath) + delimiter + process.env.PATH,
};
for (const key of process.platform === "win32"
  ? ["USERNAME", "SystemRoot", "SystemDrive"]
  : ["USER"]) {
  if (process.env[key]) env[key] = process.env[key];
}
if (process.platform === "win32") env.USERPROFILE = home;

const child = spawn("command-code", ["acp"], {
  cwd: home,
  env,
  stdio: ["pipe", "pipe", "inherit"],
});
const lines = createInterface({ input: child.stdout });
const pending = new Map();
let failure;
let nextId = 0;

function fail(error) {
  failure = error;
  for (const { reject } of pending.values()) reject(error);
}
child.on("error", fail);
child.stdin.on("error", fail);
child.on("exit", (code, signal) => fail(new Error(`CLI exited: ${code ?? signal}`)));
lines.on("line", (line) => {
  try {
    const message = JSON.parse(line);
    assert.equal(message.jsonrpc, "2.0");
    pending.get(message.id)?.resolve(message);
  } catch (error) {
    fail(error);
  }
});

async function request(method, params) {
  if (failure) throw failure;
  const id = ++nextId;
  let timer;
  try {
    return await new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      timer = setTimeout(() => reject(new Error(`${method} timed out`)), 30_000);
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  } finally {
    clearTimeout(timer);
    pending.delete(id);
  }
}

try {
  // Same handshake and capabilities as AcpConnection::initialize.
  const init = await request("initialize", {
    protocolVersion: 1,
    clientCapabilities: {},
    clientInfo: { name: "openab", version: "0.1.0" },
  });
  assert.equal(init.error, undefined);
  assert.equal(init.result.protocolVersion, 1);
  assert.equal(init.result.agentInfo.name, "Command Code");
  assert.equal(init.result.agentCapabilities.loadSession, true);
  console.log(`PASS initialize: Command Code ${init.result.agentInfo.version}`);

  const relative = await request("session/new", { cwd: ".", mcpServers: [] });
  assert.equal(relative.error?.code, -32602);
  assert.match(relative.error.message, /absolute/i);
  console.log("PASS session/new: relative project directories are rejected");

  const session = await request("session/new", { cwd: home, mcpServers: [] });
  assert.equal(session.error?.code, -32000);
  assert.match(session.error.message, /not authenticated/i);
  console.log("PASS session/new: structured authentication error on clean HOME");
} finally {
  lines.close();
  child.stdin.end();
  if (child.exitCode === null && child.signalCode === null) {
    await new Promise((resolve) => {
      const timer = setTimeout(() => child.kill("SIGKILL"), 5_000);
      child.once("close", () => { clearTimeout(timer); resolve(); });
      child.kill();
    });
  }
  await rm(home, { recursive: true, force: true });
}
