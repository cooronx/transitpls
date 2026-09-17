const assert = require('node:assert/strict');
const { test } = require('node:test');
const { prepareRelease, previousRelease, releaseNotes } = require('./prepare-release.cjs');

test('selects the latest published stable release in the current history', () => {
  const releases = [
    { tag_name: 'v1', published_at: '2026-01-01' },
    { tag_name: 'v2', published_at: '2026-02-01' },
    { tag_name: 'v3-beta', published_at: '2026-03-01', prerelease: true },
    { tag_name: 'v3-draft', published_at: '2026-04-01', draft: true },
    { tag_name: 'other-branch', published_at: '2026-05-01' },
    { tag_name: 'current', published_at: '2026-06-01' },
  ];
  assert.equal(previousRelease(releases, 'current', (tag) => tag !== 'other-branch').tag_name, 'v2');
  assert.equal(previousRelease([], 'v1', () => true), undefined);
});

test('groups all commits and links the complete range, including first and empty releases', () => {
  const notes = releaseNotes(
    'abcdef123\nfix(parser): handle ruby\0fedcba321\nfeat!: new format\0aabbcc123\nUpdate build\0',
    'v1', 'v2', 'https://github.com/example/app',
  );
  assert.match(notes, /## 问题修复\n\n- parser：handle ruby/);
  assert.match(notes, /## 新增功能\n\n- \*\*不兼容变更\*\*：new format/);
  assert.match(notes, /## 其他变更\n\n- Update build/);
  assert.match(notes, /commit\/abcdef123/);
  assert.match(notes, /compare\/v1\.\.\.v2/);
  assert.match(releaseNotes('', 'v1', 'v2', 'url'), /没有新增提交/);
  assert.match(releaseNotes('', undefined, 'v1', 'url'), /首次发布[\s\S]*url\/commits\/v1/);
});

test('re-runs preserve edited drafts and reject already published releases', async () => {
  const release = { id: 42, tag_name: 'v2', draft: true, body: 'Edited notes' };
  const outputs = {};
  const args = {
    github: { paginate: async () => [release], rest: { repos: { listReleases: {} } } },
    context: { ref: 'refs/tags/v2', repo: { owner: 'example', repo: 'app' } },
    core: { setOutput: (name, value) => { outputs[name] = value; } },
  };
  await prepareRelease(args);
  assert.equal(outputs.release_id, 42);
  assert.equal(release.body, 'Edited notes');
  release.draft = false;
  await assert.rejects(prepareRelease(args), /已正式发布/);
});
