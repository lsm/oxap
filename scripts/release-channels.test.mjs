import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { bundledNpmPackages, npmDistTag, npmTarball, pep440, publishOrder } from "./release-channels.mjs";

test("run from a checkout path holding a space, the script still runs and refuses an unknown action", () => {
  const spaced = join(mkdtempSync(join(tmpdir(), "release-channels-")), "a dir with a space");
  mkdirSync(spaced);
  const script = join(spaced, "release-channels.mjs");
  copyFileSync(fileURLToPath(new URL("./release-channels.mjs", import.meta.url)), script);
  const result = spawnSync(process.execPath, [script, "no-such-action"], { encoding: "utf8" });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /usage: release-channels\.mjs/);
});

test("a platform package the build withheld is not selected for npm, and the rest keep their order", () => {
  const bundle = mkdtempSync(join(tmpdir(), "release-channels-"));
  mkdirSync(join(bundle, "npm"));
  const version = "0.1.0-alpha.33";
  for (const name of ["@oxap/cli-linux-arm64", "@oxap/cli-linux-x64", "@oxap/cli-win32-x64", "@oxap/cli-win32-arm64", "@oxap/cli", "@oxap/sdk", "@oxap/client"]) {
    writeFileSync(join(bundle, "npm", npmTarball(name, version)), "");
  }
  assert.deepEqual(bundledNpmPackages(version, bundle), [
    "@oxap/cli-linux-arm64",
    "@oxap/cli-linux-x64",
    "@oxap/cli-win32-x64",
    "@oxap/cli-win32-arm64",
    "@oxap/cli",
    "@oxap/sdk",
    "@oxap/client",
  ]);
});

const crate = (name, deps = [], publish = null) => ({
  name,
  version: "0.1.0-alpha.33",
  publish,
  manifest_path: `/work/oxap/rust/${name}/Cargo.toml`,
  dependencies: deps.map(([dep, kind = null]) => ({ name: dep, kind })),
});

test("a crate is published after every workspace crate it depends on", () => {
  const order = publishOrder([
    crate("oxap-serve", [["oxap-adapter"], ["oxap-protocol"]]),
    crate("oxap-adapter", [["oxap-protocol"], ["serde"]]),
    crate("oxap-protocol"),
    crate("oxap-sdk"),
  ]).map((entry) => entry.name);
  assert.deepEqual(order, ["oxap-protocol", "oxap-adapter", "oxap-sdk", "oxap-serve"]);
});

test("an unpublishable crate is left out, and a dev-dependency on one is allowed", () => {
  const order = publishOrder([
    crate("oxap-sdk", [["oxap-protocol-fake", "dev"]]),
    crate("oxap-protocol-fake", [], []),
    crate("nocomment", [], []),
  ]).map((entry) => entry.name);
  assert.deepEqual(order, ["oxap-sdk"]);
});

test("the alpha line maps to its PEP 440 spelling", () => {
  assert.equal(pep440("0.1.0-alpha.33"), "0.1.0a33");
  assert.equal(pep440("0.1.0-rc.2"), "0.1.0rc2");
  assert.equal(pep440("1.0.0"), "1.0.0");
});

test("a scoped package packs to the file npm pack writes", () => {
  assert.equal(npmTarball("@oxap/cli-linux-x64", "0.1.0-alpha.33"), "oxap-cli-linux-x64-0.1.0-alpha.33.tgz");
});

test("a prerelease takes the latest dist-tag until a stable version is on npm, past the 0.0.0 placeholder", () => {
  assert.equal(npmDistTag("0.1.0-alpha.34", ["0.0.0-stage", "0.0.0", "0.1.0-alpha.33"]), "latest");
  assert.equal(npmDistTag("0.1.0-alpha.34", []), "latest");
  assert.equal(npmDistTag("0.2.0-alpha.1", ["0.0.0", "0.1.0"]), "next");
  assert.equal(npmDistTag("0.1.0", ["0.0.0", "0.1.0-alpha.34"]), "latest");
});

function distTagWithNpm(script) {
  const bin = mkdtempSync(join(tmpdir(), "release-channels-npm-"));
  writeFileSync(join(bin, "npm"), `#!/bin/sh\n${script}\n`);
  chmodSync(join(bin, "npm"), 0o755);
  const release = fileURLToPath(new URL("./release-channels.mjs", import.meta.url));
  return spawnSync(process.execPath, [release, "dist-tag", "--version", "0.2.0-alpha.1"], {
    encoding: "utf8",
    env: { PATH: `${bin}:/usr/bin:/bin` },
  });
}

test("dist-tag reads npm's not-found answer as no published version, and picks latest", () => {
  const result = distTagWithNpm(`echo '{"error":{"code":"E404","summary":"Not Found"}}'; exit 1`);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "latest\n");
});

test("dist-tag answers next when npm already holds a stable version", () => {
  const result = distTagWithNpm(`echo '["0.0.0","0.1.0","0.1.1-alpha.1"]'`);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "next\n");
});

test("dist-tag fails and prints no tag when the npm lookup itself fails", () => {
  const result = distTagWithNpm(`echo 'npm error code ECONNRESET' >&2; exit 1`);
  assert.notEqual(result.status, 0);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /release-channels: /);
});

test("dist-tag fails and prints no tag when npm answers something that is not a version list", () => {
  const result = distTagWithNpm(`echo '<html>'`);
  assert.notEqual(result.status, 0);
  assert.equal(result.stdout, "");
});
