// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

// Usage: node scripts/smoke-native-lsp.mjs <native-server-executable>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { once } from "node:events";

assert.ok(process.argv[2], "Pass the native server executable path");
const root = await mkdtemp(path.join(tmpdir(), "hl032-lsp-"));
const file = path.join(root, "AGENTS.md");
const line = "adoption, rejection, assumptions, and source links.";
const text = `${line}\n${line}\n`;
await writeFile(file, text);
const child = spawn(path.resolve(process.argv[2]), [], { stdio: ["pipe", "pipe", "pipe"] });
let stderr = "";
child.stderr.on("data", data => { stderr += data; });
let buffer = Buffer.alloc(0);
const messages = [];
const waiters = [];
let failure;
const fail = error => {
  failure = error;
  for (const waiter of waiters.splice(0)) waiter.reject(error);
};
child.on("error", fail);
child.on("exit", code => fail(new Error(`Server exited (${code}): ${stderr}`)));
child.stdout.on("data", data => {
  buffer = Buffer.concat([buffer, data]);
  try {
    while (true) {
      const end = buffer.indexOf("\r\n\r\n");
      if (end < 0) return;
      const length = /Content-Length:\s*(\d+)/i.exec(buffer.subarray(0, end).toString());
      assert.ok(length, "Invalid LSP header");
      const size = Number(length[1]);
      if (buffer.length < end + 4 + size) return;
      const message = JSON.parse(buffer.subarray(end + 4, end + 4 + size).toString());
      buffer = buffer.subarray(end + 4 + size);
      const index = waiters.findIndex(waiter => waiter.matches(message));
      if (index < 0) messages.push(message);
      else waiters.splice(index, 1)[0].resolve(message);
    }
  } catch (error) { fail(error); }
});
function receive(matches) {
  const index = messages.findIndex(matches);
  if (index >= 0) return Promise.resolve(messages.splice(index, 1)[0]);
  if (failure) return Promise.reject(failure);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`Timed out waiting for LSP: ${stderr}`)), 20000);
    waiters.push({ matches, resolve: value => { clearTimeout(timer); resolve(value); },
      reject: error => { clearTimeout(timer); reject(error); } });
  });
}
function send(message) {
  const body = JSON.stringify({ jsonrpc: "2.0", ...message });
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

try {
  const uri = pathToFileURL(file).href;
  const diagnosticMessage = message => message.method === "textDocument/publishDiagnostics" && message.params.uri === uri;
  send({ id: 1, method: "initialize", params: {
    processId: process.pid, capabilities: {},
    workspaceFolders: [{ uri: pathToFileURL(root).href, name: "duplicate-test" }],
  } });
  const initialized = await receive(message => message.id === 1);
  assert.ok(initialized.result, JSON.stringify(initialized));
  send({ method: "initialized", params: {} });
  send({ method: "textDocument/didOpen", params: {
    textDocument: { uri, languageId: "markdown", version: 1, text },
  } });
  const published = await receive(diagnosticMessage);
  const warning = published.params.diagnostics.find(diagnostic => diagnostic.code === "HL032");
  assert.ok(warning, `No HL032 warning: ${JSON.stringify(published)}`);
  assert.equal(warning.severity, 2);
  assert.equal(warning.range.start.line, 1);
  assert.match(warning.message, /assumption:/);
  // Equivalent file URIs may encode the Windows drive colon differently.
  assert.equal(fileURLToPath(warning.relatedInformation[0].location.uri), file);
  assert.equal(warning.relatedInformation[0].location.range.start.line, 0);
  console.log("HL032 warning: line 2, related line 1, normalization evidence present");
  send({ id: 2, method: "harnessLens/workspaceReport", params: {
    rootUri: pathToFileURL(root).href,
  } });
  const workspaceReport = await receive(message => message.id === 2);
  assert.equal(workspaceReport.result.schemaVersion, 1);
  assert.equal(workspaceReport.result.reports.length, 1);
  const report = workspaceReport.result.reports[0];
  assert.ok(report.sources.some(source => source.path === "AGENTS.md"));
  assert.ok(report.metrics.some(metric =>
    metric.path === "AGENTS.md" && metric.name === "harness.source.estimated_tokens"));
  assert.ok(!JSON.stringify(workspaceReport.result).includes(line));
  console.log("Workspace report: per-file metrics present, source content absent");
  send({ method: "textDocument/didChange", params: {
    textDocument: { uri, version: 2 }, contentChanges: [{ text: `${line}\n` }],
  } });
  const cleared = await receive(diagnosticMessage);
  assert.ok(!cleared.params.diagnostics.some(diagnostic => diagnostic.code === "HL032"));
  console.log("HL032 cleared after removing the duplicate in the unsaved editor buffer");
  send({ id: 3, method: "shutdown", params: null });
  await receive(message => message.id === 3);
  send({ method: "exit" });
} finally {
  if (child.exitCode === null && child.pid) {
    const exited = once(child, "exit");
    child.kill();
    await exited;
  }
  await rm(root, { recursive: true, force: true });
}
