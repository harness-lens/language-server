import assert from "node:assert/strict";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { TextDocument } from "vscode-languageserver-textdocument";
import { findingToDiagnostic, validateTextDocument } from "../dist/index.js";

test("maps evidence findings to LSP diagnostics", () => {
  const diagnostic = findingToDiagnostic({
    severity: "fail",
    ruleId: "HL031",
    message: "Conflicting instructions",
    file: "/repo/AGENTS.md",
    line: 4,
    evidence: "Always test <> Never test",
  });
  assert.equal(diagnostic?.severity, 1);
  assert.equal(diagnostic?.range.start.line, 3);
  assert.equal(diagnostic?.code, "HL031");
});

test("validates recognized harness documents only", () => {
  const harness = TextDocument.create(
    pathToFileURL("/repo/AGENTS.md").href,
    "markdown",
    1,
    "# Instructions\n- Maybe update dependencies\n",
  );
  const diagnostics = validateTextDocument(harness);
  assert.ok(diagnostics.some((item) => item.code === "HL014"));
  assert.ok(diagnostics.some((item) => item.code === "HL021"));

  const ordinary = TextDocument.create(pathToFileURL("/repo/README.md").href, "markdown", 1, "# Readme");
  assert.deepEqual(validateTextDocument(ordinary), []);
});
