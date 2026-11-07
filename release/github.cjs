// GitHub receives only public assets. Publication requires an explicit manual request.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

function hash(data) {
  return `sha256:${crypto.createHash('sha256').update(data).digest('hex')}`;
}

function version(tag) {
  if (!/^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(tag)) {
    throw new Error(`Expected a stable semantic version: ${tag}`);
  }
  return tag.slice(1).split('.').map(BigInt);
}

function compare(left, right) {
  const a = version(left), b = version(right);
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  return 0;
}

async function optional(call) {
  try { return (await call()).data; }
  catch (error) { if (error.status !== 404) throw error; }
}

function assertContext(context, config, publish) {
  if (`${context.repo.owner}/${context.repo.repo}` !== config.repository ||
      !['workflow_dispatch', 'push'].includes(context.eventName) || context.ref !== 'refs/heads/main' ||
      !/^[a-f0-9]{40}$/.test(context.sha)) {
    throw new Error('Run this release workflow manually from the canonical main branch.');
  }
  if (publish && (context.eventName !== 'workflow_dispatch' ||
      (context.payload.inputs?.publish !== 'true' && context.payload.inputs?.publish !== true))) {
    throw new Error('Publication needs an explicit publish=true workflow input.');
  }
}

async function assertNewVersion(github, repo, tag) {
  const existing = await optional(() => github.rest.repos.getReleaseByTag({ ...repo, tag }));
  if (existing) throw new Error('This version already has a release or draft. Prepare a new version.');
  // Paginate so the result does not depend on creation dates or GitHub asset ordering.
  const releases = await github.paginate(github.rest.repos.listReleases, { ...repo, per_page: 100 });
  for (const release of releases) {
    if (!release.draft && !release.prerelease && /^v\d+\.\d+\.\d+$/.test(release.tag_name) &&
        compare(tag, release.tag_name) <= 0) {
      throw new Error(`Version ${tag} is not newer than published ${release.tag_name}.`);
    }
  }
}

async function assertTag(github, repo, tag, sha) {
  let target = (await optional(() => github.rest.git.getRef({ ...repo, ref: `tags/${tag}` })))?.object;
  if (!target) return;
  for (let i = 0; target.type === 'tag' && i < 8; i++) {
    target = (await github.rest.git.getTag({ ...repo, tag_sha: target.sha })).data.object;
  }
  if (target.type !== 'commit' || target.sha !== sha) throw new Error('The version tag points to another commit.');
}

async function preflight({ github, context, core }) {
  const config = JSON.parse(fs.readFileSync('release/config.json', 'utf8'));
  const info = JSON.parse(process.env.RELEASE_METADATA);
  const publish = process.env.RELEASE_PUBLISH === 'true';
  assertContext(context, config, publish);
  version(info.tag);
  if (info.repository !== config.repository || info.commit !== context.sha || info.tag !== `v${info.version}`) {
    throw new Error('Source version and checked commit do not match.');
  }
  if (publish) {
    await assertNewVersion(github, context.repo, info.tag);
    await assertTag(github, context.repo, info.tag, context.sha);
  }
  core.notice(`${publish ? 'New release' : 'Build-only verification'}: ${info.tag} at ${context.sha}`);
}

async function publish({ github, context, core }) {
  const config = JSON.parse(fs.readFileSync('release/config.json', 'utf8'));
  assertContext(context, config, true);
  const directory = '.release-stage';
  const manifest = JSON.parse(fs.readFileSync(`${directory}/_manifest.json`, 'utf8'));
  version(manifest.tag);
  if (manifest.repository !== config.repository || manifest.product !== config.product || manifest.commit !== context.sha ||
      manifest.version !== process.env.RELEASE_VERSION ||
      manifest.tag !== `v${manifest.version}` || manifest.notes !== `release/notes/${manifest.version}.md`) {
    throw new Error('Release metadata is bound to another source or version.');
  }
  const notes = fs.readFileSync(manifest.notes, 'utf8');
  if (notes.trim().length < 100 || !notes.includes(manifest.version) ||
      hash(Buffer.from(notes)) !== `sha256:${manifest.notes_sha256}`) {
    throw new Error('Reviewed release notes changed after staging.');
  }
  if (!Array.isArray(manifest.assets) || !manifest.assets.length) throw new Error('No release assets.');
  const names = new Set();
  for (const asset of manifest.assets) {
    if (!asset.name || /[\\/\x00-\x1f]/.test(asset.name) || asset.name === '.' || asset.name === '..' ||
        asset.name === '_manifest.json' || names.has(asset.name)) throw new Error('Unsafe or duplicate asset name.');
    names.add(asset.name);
    const file = path.join(directory, asset.name);
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error('A release asset is not a regular file.');
    const data = fs.readFileSync(file);
    if (!data.length || data.length !== asset.size || hash(data) !== asset.digest) throw new Error(`Asset changed: ${asset.name}`);
  }
  const expected = config.assets.map(a => a.name.replaceAll('{version}', manifest.version));
  if (config.notices) expected.push(`${config.product}-v${manifest.version}-notices.zip`);
  expected.push('SHA256SUMS.txt', ...(config.checksum_aliases || []).map(n => n.replaceAll('{version}', manifest.version)));
  if (expected.length !== names.size || expected.some(n => !names.has(n)) ||
      fs.readdirSync(directory).some(n => n !== '_manifest.json' && !names.has(n))) {
    throw new Error('The release candidate contains missing checksums or unexpected files.');
  }
  await assertNewVersion(github, context.repo, manifest.tag);
  await assertTag(github, context.repo, manifest.tag, context.sha);
  const marker = `<!-- release-from-github:${context.sha}:${context.runId}:${hash(Buffer.from(JSON.stringify(manifest)))} -->`;
  const body = `${notes.trim()}\n\n${marker}\n`;
  // A failed upload leaves an inspectable draft; never clobber an existing release or draft.
  const draft = (await github.rest.repos.createRelease({ ...context.repo,
    tag_name: manifest.tag, target_commitish: context.sha, name: `${manifest.product} ${manifest.tag}`,
    body, draft: true, prerelease: false,
  })).data;
  for (const asset of manifest.assets) {
    const data = fs.readFileSync(path.join(directory, asset.name));
    if (hash(data) !== asset.digest) throw new Error('Asset changed before upload; release remains a draft.');
    const saved = (await github.rest.repos.uploadReleaseAsset({ ...context.repo, release_id: draft.id,
      name: asset.name, data, headers: { 'content-type': 'application/octet-stream', 'content-length': data.length },
    })).data;
    if (saved.name !== asset.name || saved.size !== asset.size || saved.digest !== asset.digest) {
      throw new Error('GitHub asset verification failed; release remains a draft.');
    }
  }
  const ready = (await github.rest.repos.getRelease({ ...context.repo, release_id: draft.id })).data;
  if (!ready.draft || ready.target_commitish !== context.sha || ready.body !== body || ready.tag_name !== manifest.tag ||
      ready.assets.length !== manifest.assets.length || ready.assets.some(saved => {
        const asset = manifest.assets.find(a => a.name === saved.name);
        return !asset || asset.size !== saved.size || asset.digest !== saved.digest;
      })) throw new Error('The uploaded draft differs from the tested candidate.');
  await assertTag(github, context.repo, manifest.tag, context.sha);
  const result = (await github.rest.repos.updateRelease({ ...context.repo, release_id: draft.id,
    draft: false, prerelease: false, make_latest: 'true',
  })).data;
  if (result.draft || result.tag_name !== manifest.tag) throw new Error('Publication did not complete.');
  core.notice(`Published ${result.html_url}`);
}

module.exports = { preflight, publish, compare, assertContext, assertTag };
