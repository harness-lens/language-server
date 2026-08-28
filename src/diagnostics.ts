import { fileURLToPath } from "node:url";
import path from "node:path";
import {
  classifyHarnessPath,
  parseHarness,
  validateHarnesses,
} from "@harness-lens/core";
import type { Finding, HarnessFile } from "@harness-lens/core";
import { DiagnosticSeverity } from "vscode-languageserver/node";
import type { Diagnostic } from "vscode-languageserver/node";
import type { TextDocument } from "vscode-languageserver-textdocument";

export function findingToDiagnostic(finding: Finding): Diagnostic | null {
  if (finding.severity === "pass") return null;
  const line = Math.max((finding.line ?? 1) - 1, 0);
  return {
    severity: finding.severity === "fail" ? DiagnosticSeverity.Error : DiagnosticSeverity.Warning,
    range: {
      start: { line, character: 0 },
      end: { line, character: Math.max(finding.evidence?.length ?? 1, 1) },
    },
    code: finding.ruleId,
    source: "harness-lens",
    message: finding.evidence ? `${finding.message}: ${finding.evidence}` : finding.message,
  };
}

export function validateTextDocument(document: TextDocument): Diagnostic[] {
  if (!document.uri.startsWith("file:")) return [];
  const filePath = fileURLToPath(document.uri);
  const kind = classifyHarnessPath(filePath.replaceAll(path.sep, "/"));
  if (!kind) return [];

  const file: HarnessFile = {
    path: filePath,
    kind,
    scope: path.dirname(filePath),
    content: document.getText(),
    bytes: Buffer.byteLength(document.getText(), "utf8"),
  };
  return validateHarnesses([parseHarness(file)], file.scope)
    .map(findingToDiagnostic)
    .filter((diagnostic): diagnostic is Diagnostic => diagnostic !== null);
}
