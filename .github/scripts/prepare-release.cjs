const { execFileSync } = require('node:child_process');

function git(...args) {
  return execFileSync('git', args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
}

function previousRelease(releases, tag, isAncestor) {
  return releases
    .filter((release) => !release.draft && !release.prerelease && release.tag_name !== tag)
    .sort((a, b) => Date.parse(b.published_at) - Date.parse(a.published_at))
    .find((release) => isAncestor(release.tag_name));
}

function releaseNotes(log, previous, tag, url) {
  const groups = new Map([
    ['feat', ['新增功能', []]],
    ['fix', ['问题修复', []]],
    ['refactor', ['内部改进', []]],
    ['docs', ['文档更新', []]],
    ['other', ['其他变更', []]],
  ]);
  for (const entry of log.split('\0').filter((entry) => entry.trim())) {
    const [sha, ...lines] = entry.trim().split('\n');
    const subject = lines.join(' ');
    const match = /^(\w+)(?:\(([^)]+)\))?(!)?:\s+(.+)$/.exec(subject);
    const type = match?.[1];
    const title = match
      ? `${match[3] ? '**不兼容变更**：' : ''}${match[2] ? `${match[2]}：` : ''}${match[4]}`
      : subject;
    groups.get(groups.has(type) ? type : 'other')[1].push(
      `- ${title} ([${sha.slice(0, 7)}](${url}/commit/${sha}))`,
    );
  }
  const sections = [...groups.values()]
    .filter(([, entries]) => entries.length)
    .map(([title, entries]) => `## ${title}\n\n${entries.join('\n')}`);
  if (!sections.length) sections.push('本版本没有新增提交。');
  const link = previous
    ? `${url}/compare/${encodeURIComponent(previous)}...${encodeURIComponent(tag)}`
    : `${url}/commits/${encodeURIComponent(tag)}`;
  return `${previous ? `相较于 ${previous} 的变更。` : '首次发布。'}\n\n${sections.join('\n\n')}\n\n[完整变更](${link})\n`;
}

async function prepareRelease({ github, context, core }) {
  const tag = context.ref.replace(/^refs\/tags\//, '');
  const releases = await github.paginate(github.rest.repos.listReleases, {
    ...context.repo, per_page: 100,
  });
  const existing = releases.find((release) => release.tag_name === tag);
  if (existing) {
    if (!existing.draft) throw new Error(`${tag} 已正式发布，请使用新 tag。`);
    // Re-runs must preserve release notes already edited by the author.
    core.setOutput('release_id', existing.id);
    return;
  }
  const previous = previousRelease(releases, tag, (candidate) => {
    try {
      git('merge-base', '--is-ancestor', `refs/tags/${candidate}`, `refs/tags/${tag}`);
      return true;
    } catch (error) {
      if (error.status === 1) return false;
      throw error;
    }
  });
  const range = previous
    ? `refs/tags/${previous.tag_name}..refs/tags/${tag}`
    : `refs/tags/${tag}`;
  const log = git('log', '--reverse', '--format=%H%n%s', '-z', range, '--');
  const url = `${context.serverUrl}/${context.repo.owner}/${context.repo.repo}`;
  const { data: release } = await github.rest.repos.createRelease({
    ...context.repo,
    tag_name: tag,
    name: `transitpls ${tag}`,
    body: releaseNotes(log, previous?.tag_name, tag, url),
    draft: true,
    prerelease: false,
  });
  core.setOutput('release_id', release.id);
}

module.exports = { prepareRelease, previousRelease, releaseNotes };
