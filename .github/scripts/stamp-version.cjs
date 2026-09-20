const fs = require('node:fs');
const path = require('node:path');

const SEMVER = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;

function versionFromRef(ref) {
  const version = String(ref || '')
    .trim()
    .replace(/^refs\/tags\//, '')
    .replace(/^v/, '');
  if (!SEMVER.test(version)) {
    throw new Error(`tag "${ref}" does not contain a valid version`);
  }
  return version;
}

function replaceVersion(file, pattern, version) {
  const source = fs.readFileSync(file, 'utf8');
  if (!pattern.test(source)) {
    throw new Error(`version field not found in ${file}`);
  }
  const next = source.replace(pattern, (match, prefix) => `${prefix}"${version}"`);
  if (next !== source) fs.writeFileSync(file, next);
}

// 以 tag 为准，把版本号写进前端、Tauri 与 Cargo 清单。
function stampVersion({ ref, root }) {
  const version = versionFromRef(ref);
  replaceVersion(path.join(root, 'package.json'), /("version":\s*)"[^"]*"/, version);
  replaceVersion(
    path.join(root, 'src-tauri/tauri.conf.json'),
    /("version":\s*)"[^"]*"/,
    version,
  );
  replaceVersion(
    path.join(root, 'src-tauri/Cargo.toml'),
    /(\[package\][\s\S]*?\nversion = )"[^"]*"/,
    version,
  );
  return version;
}

if (require.main === module) {
  const ref = process.argv[2] || process.env.GITHUB_REF_NAME;
  if (!ref) {
    console.error('usage: node .github/scripts/stamp-version.cjs <tag>');
    process.exit(1);
  }
  try {
    const version = stampVersion({ ref, root: path.resolve(__dirname, '..', '..') });
    console.log(`stamped version ${version} from ${ref}`);
  } catch (error) {
    console.error(`stamp-version: ${error.message}`);
    process.exit(1);
  }
}

module.exports = { stampVersion, versionFromRef };
