/**
 * Unit tests for platform.ts — pure utility functions only.
 *
 * These tests do NOT require a live VS Code instance.
 * They use manual property stubs on Node's `os` and `fs` modules so they
 * run anywhere without a VS Code binary.
 */

import * as assert from "assert";
import * as os from "os";
import * as fs from "fs";
import * as path from "path";

// ---------------------------------------------------------------------------
// Helpers for lightweight stubbing (no sinon dep needed)
// ---------------------------------------------------------------------------

type Stub<T> = { restore: () => void } & { returns: (v: T) => void };

function stubMethod<O extends object, K extends keyof O>(
  obj: O,
  method: K,
  value: unknown
): { restore: () => void } {
  const original = obj[method];
  (obj as Record<string, unknown>)[method as string] = () => value;
  return { restore: () => { obj[method] = original; } };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

suite("platform.ts — detectPlatform", () => {
  // We need to re-require the module after patching os.platform/os.arch so
  // the function picks up the stub. Node caches modules, so we delete the
  // cache entry before each require.
  function freshDetectPlatform() {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    delete require.cache[require.resolve("../platform")];
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require("../platform").detectPlatform;
  }

  let platformStub: { restore: () => void };
  let archStub: { restore: () => void };

  teardown(() => {
    platformStub?.restore();
    archStub?.restore();
  });

  test("linux x64 — correct binary name and requiresChmod=true", () => {
    platformStub = stubMethod(os, "platform", "linux");
    archStub = stubMethod(os, "arch", "x64");
    const info = freshDetectPlatform()();
    assert.strictEqual(info.binaryName, "crosslink-linux");
    assert.strictEqual(info.requiresChmod, true);
  });

  test("linux arm64 — binary name has -arm64 suffix", () => {
    platformStub = stubMethod(os, "platform", "linux");
    archStub = stubMethod(os, "arch", "arm64");
    const info = freshDetectPlatform()();
    assert.strictEqual(info.binaryName, "crosslink-linux-arm64");
  });

  test("darwin x64 — correct binary name", () => {
    platformStub = stubMethod(os, "platform", "darwin");
    archStub = stubMethod(os, "arch", "x64");
    const info = freshDetectPlatform()();
    assert.strictEqual(info.binaryName, "crosslink-darwin");
    assert.strictEqual(info.requiresChmod, true);
  });

  test("win32 x64 — binary has .exe extension, requiresChmod=false", () => {
    platformStub = stubMethod(os, "platform", "win32");
    archStub = stubMethod(os, "arch", "x64");
    const info = freshDetectPlatform()();
    assert.strictEqual(info.binaryName, "crosslink-win.exe");
    assert.strictEqual(info.requiresChmod, false);
  });

  test("unsupported platform throws", () => {
    platformStub = stubMethod(os, "platform", "freebsd");
    archStub = stubMethod(os, "arch", "x64");
    assert.throws(() => freshDetectPlatform()(), /Unsupported platform/);
  });

  test("unsupported architecture throws", () => {
    platformStub = stubMethod(os, "platform", "linux");
    archStub = stubMethod(os, "arch", "ia32");
    assert.throws(() => freshDetectPlatform()(), /Unsupported architecture/);
  });
});

suite("platform.ts — resolveBinaryPath with override", () => {
  let existsStub: { restore: () => void };

  function freshResolveBinaryPath() {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    delete require.cache[require.resolve("../platform")];
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return require("../platform").resolveBinaryPath;
  }

  teardown(() => {
    existsStub?.restore();
  });

  test("returns resolved path when override file exists", () => {
    existsStub = stubMethod(fs, "existsSync", true);
    const result = freshResolveBinaryPath()("/ext", "/usr/local/bin/crosslink");
    assert.ok(path.isAbsolute(result));
    assert.ok(result.includes("crosslink"));
  });

  test("throws when override file does not exist", () => {
    existsStub = stubMethod(fs, "existsSync", false);
    assert.throws(
      () => freshResolveBinaryPath()("/ext", "/nonexistent/crosslink"),
      /Configured binary not found/
    );
  });

  test("ignores empty-string override (uses bundled binary path logic)", () => {
    // When override is empty, the function falls through to bundled binary.
    // We stub existsSync to return false for the bundled path to confirm
    // the expected error (bundled binary missing) rather than the override error.
    existsStub = stubMethod(fs, "existsSync", false);
    // Need os stubs too so detectPlatform() inside resolveBinaryPath works
    const platformStub = stubMethod(os, "platform", "linux");
    const archStub = stubMethod(os, "arch", "x64");
    try {
      assert.throws(
        () => freshResolveBinaryPath()("/ext", ""),
        /Bundled binary not found/
      );
    } finally {
      platformStub.restore();
      archStub.restore();
    }
  });
});
