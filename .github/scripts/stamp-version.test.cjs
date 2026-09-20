const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { stampVersion, versionFromRef } = require('./stamp-version.cjs');

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'stamp-version-'));
  fs.mkdirSync(path.join(root, 'src-tauri'));
  fs.writeFileSync(
    path.join(root, 'package.json'),
    '{\n  "name": "app",\n  "version": "0.1.0"\n}\n',
  );
  fs.writeFileSync(
    path.join(root, 'src-tauri/tauri.conf.json'),
    '{\n  "productName": "app",\n  "version": "0.1.0"\n}\n',
  );
  fs.writeFileSync(
    path.join(root, 'src-tauri/Cargo.toml'),
    '[package]\nname = "app"\nversion = "0.1.0"\n\n[dependencies]\nserde = { version = "1", features = ["derive"] }\n',
  );
  return root;
}

test('derives a version from the tag and rejects anything else', () => {
  assert.equal(versionFromRef('v1.2.3'), '1.2.3');
  assert.equal(versionFromRef('v0.2.0-beta.1'), '0.2.0-beta.1');
  assert.throws(() => versionFromRef('release'), /valid version/);
  assert.throws(() => versionFromRef('v1.2'), /valid version/);
});

test('stamps only the package version fields', () => {
  const root = fixture();
  assert.equal(stampVersion({ ref: 'v0.2.0', root }), '0.2.0');
  assert.match(fs.readFileSync(path.join(root, 'package.json'), 'utf8'), /"version": "0.2.0"/);
  const cargo = fs.readFileSync(path.join(root, 'src-tauri/Cargo.toml'), 'utf8');
  assert.match(cargo, /\[package\][\s\S]*?version = "0.2.0"/);
  assert.match(cargo, /serde = \{ version = "1"/);
  assert.equal(stampVersion({ ref: 'v0.2.0', root }), '0.2.0');
  fs.rmSync(root, { recursive: true, force: true });
});
