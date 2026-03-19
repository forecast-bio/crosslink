import { defineConfig } from "@vscode/test-cli";

export default defineConfig({
  files: "out/test/**/*.test.js",
  // Unit tests for pure utility functions run without a VS Code instance.
  // Integration tests that exercise VS Code APIs (extension activation,
  // command registration, etc.) require a live VS Code binary and are
  // marked with the @integration tag — run them via `vscode-test --label integration`.
  workspaceFolder: "./",
  mocha: {
    timeout: 10000,
  },
});
