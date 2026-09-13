// SPDX-License-Identifier: MPL-2.0
// Copyright © 2026 Cristian Camargo Filho

// Usage: node scripts/smoke-native-lsp.mjs <native-server-executable>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { realpathSync } from "node:fs";
import { mkdtemp, realpath, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { once } from "node:events";

assert.ok(process.argv[2], "Pass the native server executable path");
const root = await mkdtemp(path.join(tmpdir(), "hl032-lsp-"));
const file = path.join(root, "AGENTS.md");
const closedFile = path.join(root, "CLAUDE.md");
const line = "adoption, rejection, assumptions, and source links.";
const text = `${line}\n${line}\n`;
await writeFile(file, text);
await writeFile(closedFile, "Use use tests.\n");
const child = spawn(path.resolve(process.argv[2]), [], {
  env: {
    ...process.env,
    HARNESS_LENS_WORKSPACE_TRUSTED: "false",
    HARNESS_METRICS_MODE: "off",
  },
  stdio: ["pipe", "pipe", "pipe"],
});
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
  const canonicalFile = await realpath(file);
  const canonicalClosedFile = await realpath(closedFile);
  // Closed files use server-generated URIs. Rust and Node may encode the
  // Windows drive colon differently, so match file identity instead of URI text.
  const diagnosticsFor = canonicalPath => message =>
    message.method === "textDocument/publishDiagnostics"
    && realpathSync.native(fileURLToPath(message.params.uri)) === canonicalPath;
  const diagnosticMessage = diagnosticsFor(canonicalFile);
  const closedDiagnosticMessage = diagnosticsFor(canonicalClosedFile);
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
  assert.equal(
    await realpath(fileURLToPath(warning.relatedInformation[0].location.uri)),
    await realpath(file),
  );
  assert.equal(warning.relatedInformation[0].location.range.start.line, 0);
  console.log("HL032 warning: line 2, related line 1, normalization evidence present");
  const closedPublished = await receive(closedDiagnosticMessage);
  const closedWarning = closedPublished.params.diagnostics.find(diagnostic => diagnostic.code === "HL010");
  assert.ok(closedWarning, `No closed-file HL010 warning: ${JSON.stringify(closedPublished)}`);
  assert.equal(closedWarning.range.start.line, 0);
  console.log("Closed-file warning: workspace finding published before the file is opened");
  send({ id: 2, method: "harnessLens/workspaceReport", params: {
    rootUri: pathToFileURL(root).href,
    maxFiles: 5000,
  } });
  const workspaceReport = await receive(message => message.id === 2);
  assert.equal(workspaceReport.result.schemaVersion, 1);
  assert.equal(workspaceReport.result.reports.length, 1);
  assert.equal(workspaceReport.result.runtime.mode, "off");
  assert.equal(workspaceReport.result.runtime.state, "off");
  const report = workspaceReport.result.reports[0];
  assert.ok(report.sources.some(source => source.path === "AGENTS.md"));
  assert.ok(report.metrics.some(metric =>
    metric.path === "AGENTS.md" && metric.name === "harness.source.estimated_tokens"));
  assert.ok(!JSON.stringify(workspaceReport.result).includes(line));
  console.log("Workspace report: per-file metrics present, source content absent");
  send({ id: 3, method: "harnessLens/providerCatalog", params: {} });
  const providerCatalog = await receive(message => message.id === 3);
  assert.equal(providerCatalog.result.schemaVersion, 1);
  const nativeProvider = providerCatalog.result.providers.find(
    provider => provider.descriptor.id === "harness-lens-native",
  );
  const codeburnProvider = providerCatalog.result.providers.find(
    provider => provider.descriptor.id === "codeburn",
  );
  assert.equal(nativeProvider.selected, true);
  assert.equal(nativeProvider.availability, "available");
  assert.equal(nativeProvider.installation, "built_in");
  assert.equal(codeburnProvider.selected, false);
  assert.equal(codeburnProvider.availability, "off");
  console.log("Provider catalog: Native available, optional provider off");
  send({ id: 4, method: "harnessLens/providerAggregate", params: {
    rootUri: pathToFileURL(root).href,
    maxFiles: 5000,
  } });
  const providerAggregate = await receive(message => message.id === 4);
  assert.equal(providerAggregate.result.schemaVersion, 1);
  assert.equal(providerAggregate.result.runtime.mode, "off");
  assert.deepEqual(providerAggregate.result.aggregate.selected_providers, ["harness-lens-native"]);
  assert.ok(providerAggregate.result.aggregate.reports["harness-lens-native"]);
  assert.ok(!providerAggregate.result.aggregate.reports.codeburn);
  assert.ok(!JSON.stringify(providerAggregate.result).includes(line));
  console.log("Provider aggregate: Native namespaced, source content absent");
  send({ id: 5, method: "harnessLens/observedFlow", params: {
    rootUri: pathToFileURL(root).href,
    maxNodes: 64,
    maxEdges: 128,
    metric: "transitions",
  } });
  const observedFlow = await receive(message => message.id === 5);
  assert.equal(observedFlow.result.schemaVersion, 1);
  assert.equal(observedFlow.result.status.state, "off");
  assert.equal(observedFlow.result.graph.kind, "observed_flow");
  assert.equal(observedFlow.result.graph.availability, "unavailable");
  assert.deepEqual(observedFlow.result.graph.nodes, []);
  assert.deepEqual(observedFlow.result.graph.edges, []);
  assert.ok(!JSON.stringify(observedFlow.result).includes(line));
  console.log("Observed flow: explicit unavailable state, source content absent");
  await writeFile(closedFile, "Use tests.\n");
  send({ method: "textDocument/didChange", params: {
    textDocument: { uri, version: 2 }, contentChanges: [{ text: `${line}\n` }],
  } });
  const cleared = await receive(diagnosticMessage);
  assert.ok(!cleared.params.diagnostics.some(diagnostic => diagnostic.code === "HL032"));
  console.log("HL032 cleared after removing the duplicate in the unsaved editor buffer");
  const closedCleared = await receive(closedDiagnosticMessage);
  assert.ok(!closedCleared.params.diagnostics.some(diagnostic => diagnostic.code === "HL010"));
  console.log("Closed-file warning cleared after the next workspace analysis");
  send({ id: 6, method: "shutdown", params: null });
  await receive(message => message.id === 6);
  send({ method: "exit" });
} finally {
  if (child.exitCode === null && child.pid) {
    const exited = once(child, "exit");
    child.kill();
    await exited;
  }
  await rm(root, { recursive: true, force: true });
}
