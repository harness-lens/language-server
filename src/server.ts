import {
  createConnection,
  ProposedFeatures,
  TextDocuments,
  TextDocumentSyncKind,
} from "vscode-languageserver/node";
import type { InitializeResult } from "vscode-languageserver/node";
import { TextDocument } from "vscode-languageserver-textdocument";
import { validateTextDocument } from "./diagnostics.js";

export function startServer(): void {
  const connection = createConnection(ProposedFeatures.all);
  const documents = new TextDocuments(TextDocument);

  connection.onInitialize((): InitializeResult => ({
    capabilities: {
      textDocumentSync: TextDocumentSyncKind.Incremental,
    },
    serverInfo: {
      name: "Harness Lens Language Server",
      version: "0.0.1",
    },
  }));

  const publish = (document: TextDocument): void => {
    connection.sendDiagnostics({
      uri: document.uri,
      version: document.version,
      diagnostics: validateTextDocument(document),
    });
  };

  documents.onDidOpen(({ document }) => publish(document));
  documents.onDidChangeContent(({ document }) => publish(document));
  documents.onDidClose(({ document }) => {
    connection.sendDiagnostics({ uri: document.uri, diagnostics: [] });
  });

  documents.listen(connection);
  connection.listen();
}
