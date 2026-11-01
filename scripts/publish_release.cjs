const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const PUBLISHER_PATHS = new Set([
  '.github/workflows/release.yml',
  'scripts/publish_release.cjs',
  'scripts/release-draft-lease.json',
  'scripts/test_publish_release.cjs',
]);

const digestOf = data => `sha256:${crypto.createHash('sha256').update(data).digest('hex')}`;
const matches = (actual, expected) => actual.name === expected.name &&
  actual.size === expected.size && actual.digest === expected.digest;

module.exports = async ({github, context, core, root = process.cwd(), env = process.env}) => {
  const repository = context.repo;
  const fullName = `${repository.owner}/${repository.repo}`;
  if (fullName !== 'siarheisinelnikau/space-sniffer') throw new Error('Unexpected release repository.');
  const tag = env.RELEASE_TAG;
  const commit = env.RELEASE_SHA;
  const cargo = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  const version = cargo.match(/^version\s*=\s*"(\d+\.\d+\.\d+)"\s*$/m)?.[1];
  if (!version || tag !== `v${version}` || !/^[0-9a-f]{40}$/.test(commit) || commit !== context.sha) {
    throw new Error('The checked commit, Cargo version and release tag must match.');
  }
  const name = `Space Sniffer ${tag}`;
  const notes = fs.readFileSync(path.join(root, `docs/release-notes/${tag}.md`), 'utf8').trim();
  if (notes.length < 100 || !notes.includes(version)) throw new Error('Missing reviewed release notes.');
  const marker = `<!-- space-sniffer-publisher:v1 ${tag} ${commit} -->`;
  const markedNotes = `${notes}\n\n${marker}`;
  const names = [`SpaceSniffer-${tag}.exe`, 'SHA256SUMS.txt'];
  const expected = names.map(assetName => {
    const data = fs.readFileSync(path.join(root, 'release', assetName));
    if (!data.length) throw new Error(`Empty release asset: ${assetName}`);
    return {name: assetName, size: data.length, digest: digestOf(data), data};
  });
  const checksum = `${expected[0].digest.slice(7)}  ${names[0]}`;
  if (expected[1].data.toString('utf8').trim() !== checksum) throw new Error('The checked executable checksum differs.');
  const lease = JSON.parse(fs.readFileSync(path.join(root, 'scripts/release-draft-lease.json'), 'utf8'));
  const activeLease = lease.tag === tag;
  if (activeLease && (lease.repository !== fullName || lease.name !== name ||
      !Number.isSafeInteger(lease.release_id) || !/^[0-9a-f]{40}$/.test(lease.target) ||
      lease.assets.length !== names.length || !names.every(assetName =>
        lease.assets.filter(asset => asset.name === assetName && asset.size > 0 && /^sha256:[0-9a-f]{64}$/.test(asset.digest)).length === 1))) {
    throw new Error('The task-owned draft lease is invalid.');
  }

  async function assertTagTarget() {
    let reference;
    try {
      reference = (await github.rest.git.getRef({...repository, ref: `tags/${tag}`})).data.object;
    } catch (error) { if (error.status === 404) return; throw error; }
    if (reference.type !== 'commit' || reference.sha !== commit) {
      throw new Error('An existing version tag belongs to another commit; tags are never moved.');
    }
  }
  await assertTagTarget();

  // Drafts without a published tag are found by numeric ID or collection, never by tag URL.
  const releases = [];
  for (let page = 1; ; page++) {
    const result = await github.rest.repos.listReleases({...repository, per_page: 100, page});
    releases.push(...result.data);
    if (result.data.length < 100) break;
    if (page >= 10) throw new Error('Cannot prove that the complete release collection was inspected.');
  }
  const tagged = releases.filter(release => release.tag_name === tag);
  if (tagged.some(release => !release.draft)) throw new Error('A published release will never be modified.');
  if (tagged.length > 1) throw new Error('Multiple drafts claim this release tag.');
  let leasedDraft;
  if (activeLease) {
    try {
      leasedDraft = (await github.rest.repos.getRelease({...repository, release_id: lease.release_id})).data;
    } catch (error) { if (error.status !== 404) throw error; }
  }
  if (leasedDraft && tagged[0] && leasedDraft.id !== tagged[0].id) throw new Error('The release draft ID does not match the lease.');
  let draft = leasedDraft || tagged[0];
  let leased = false;
  if (draft) {
    if (!draft.draft) throw new Error('A published release will never be modified.');
    if (draft.tag_name !== tag || draft.name !== name || draft.prerelease) throw new Error('The draft tag or identity differs.');
    leased = activeLease && draft.id === lease.release_id;
    const ownedCurrent = draft.target_commitish === commit && draft.body?.trim() === markedNotes;
    if (!leased && !ownedCurrent) throw new Error('An unrelated draft will never be modified.');
    if (leased && ![lease.target, commit].includes(draft.target_commitish)) throw new Error('The draft target no longer matches its lease.');
    if (leased && ![notes, markedNotes].includes(draft.body?.trim())) throw new Error('The task-owned draft notes changed.');
    const seen = new Set();
    for (const asset of draft.assets) {
      if (seen.has(asset.name)) throw new Error('Duplicate draft asset names.');
      seen.add(asset.name);
      if (!expected.some(item => matches(asset, item)) && !(leased && lease.assets.some(item => matches(asset, item)))) {
        throw new Error(`An existing draft asset differs from the task lease: ${asset.name}`);
      }
    }
    if (leased && lease.target !== commit) {
      const comparison = (await github.rest.repos.compareCommitsWithBasehead({
        ...repository, basehead: `${lease.target}...${commit}`,
      })).data;
      if (comparison.status !== 'ahead' || comparison.base_commit?.sha !== lease.target ||
          comparison.merge_base_commit?.sha !== lease.target || comparison.behind_by !== 0 ||
          !(comparison.ahead_by > 0) || !Array.isArray(comparison.files) || !comparison.files.length ||
          comparison.files.length > PUBLISHER_PATHS.size || comparison.files.some(file =>
            !PUBLISHER_PATHS.has(file.filename) || !['added', 'modified'].includes(file.status) || file.previous_filename)) {
        throw new Error('Only workflow and publisher changes may resume this task-owned draft.');
      }
    }
  }

  // All identity, source and asset leases are checked before the first mutation.
  if (!draft) {
    draft = (await github.rest.repos.createRelease({
      ...repository, tag_name: tag, target_commitish: commit, name, body: markedNotes,
      draft: true, prerelease: false,
    })).data;
  } else if (draft.target_commitish !== commit || draft.body?.trim() !== markedNotes) {
    draft = (await github.rest.repos.updateRelease({
      ...repository, release_id: draft.id, tag_name: tag, target_commitish: commit,
      body: markedNotes, draft: true, prerelease: false,
    })).data;
  }
  const releaseId = draft.id;
  for (const asset of expected) {
    const existing = draft.assets.find(item => item.name === asset.name);
    if (existing && matches(existing, asset)) continue;
    if (existing) {
      if (!leased || !lease.assets.some(item => matches(existing, item))) throw new Error('Only leased unpublished assets may be rebuilt.');
      await github.rest.repos.deleteReleaseAsset({...repository, asset_id: existing.id});
    }
    const uploaded = (await github.rest.repos.uploadReleaseAsset({
      ...repository, release_id: releaseId, name: asset.name, data: asset.data,
      headers: {'content-type': 'application/octet-stream', 'content-length': asset.size},
    })).data;
    if (!matches(uploaded, asset)) throw new Error(`Uploaded digest differs; release remains a draft: ${asset.name}`);
  }
  function assertComplete(release, shouldBeDraft) {
    if (release.id !== releaseId || release.tag_name !== tag || release.name !== name ||
        release.target_commitish !== commit || release.draft !== shouldBeDraft || release.prerelease ||
        release.assets.length !== expected.length || !expected.every(asset =>
          release.assets.filter(item => matches(item, asset)).length === 1)) {
      throw new Error('The release identity or complete verified asset set changed.');
    }
  }
  const ready = (await github.rest.repos.getRelease({...repository, release_id: releaseId})).data;
  assertComplete(ready, true);
  await assertTagTarget();
  const published = (await github.rest.repos.updateRelease({
    ...repository, release_id: releaseId, tag_name: tag, target_commitish: commit,
    draft: false, prerelease: false, make_latest: 'true',
  })).data;
  assertComplete(published, false);
  const verified = (await github.rest.repos.getRelease({...repository, release_id: releaseId})).data;
  assertComplete(verified, false);
  core.setOutput('release_id', releaseId);
  core.notice(`Published ${verified.html_url}`);
};
