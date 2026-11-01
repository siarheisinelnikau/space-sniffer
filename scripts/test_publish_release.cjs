const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const publish = require('./publish_release.cjs');

const sha = data => `sha256:${crypto.createHash('sha256').update(data).digest('hex')}`;
const copy = value => structuredClone(value);

function fixture(options = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'space-sniffer-release-'));
  fs.mkdirSync(path.join(root, 'scripts'));
  fs.mkdirSync(path.join(root, 'docs/release-notes'), {recursive: true});
  fs.mkdirSync(path.join(root, 'release'));
  const old = 'a'.repeat(40);
  const current = 'b'.repeat(40);
  const tag = 'v0.5.0';
  const notes = '# Space Sniffer v0.5.0\n\n' + 'Verified localization and saved light/dark theme controls. '.repeat(3);
  fs.writeFileSync(path.join(root, 'Cargo.toml'), '[package]\nversion = "0.5.0"\n');
  fs.writeFileSync(path.join(root, `docs/release-notes/${tag}.md`), notes);
  const asset = (name, data, id) => ({name, size: data.length, digest: sha(data), id});
  const oldExecutable = Buffer.from('Original checked executable fixture');
  const executable = Buffer.from('New checked executable fixture');
  const oldChecksum = Buffer.from(`${sha(oldExecutable).slice(7)}  SpaceSniffer-${tag}.exe\n`);
  const checksum = Buffer.from(`${sha(executable).slice(7)}  SpaceSniffer-${tag}.exe\n`);
  fs.writeFileSync(path.join(root, 'release', `SpaceSniffer-${tag}.exe`), executable);
  fs.writeFileSync(path.join(root, 'release/SHA256SUMS.txt'), checksum);
  const oldAssets = [asset(`SpaceSniffer-${tag}.exe`, oldExecutable, 11), asset('SHA256SUMS.txt', oldChecksum, 12)];
  const newAssets = [asset(`SpaceSniffer-${tag}.exe`, executable, 21), asset('SHA256SUMS.txt', checksum, 22)];
  const lease = {repository: 'siarheisinelnikau/space-sniffer', release_id: 430819184, tag,
    name: `Space Sniffer ${tag}`, target: old, assets: copy(oldAssets)};
  fs.writeFileSync(path.join(root, 'scripts/release-draft-lease.json'), JSON.stringify(lease));
  let draft = options.none ? undefined : {id: lease.release_id, tag_name: tag, name: lease.name,
    target_commitish: old, body: notes.trim(), draft: true, prerelease: false,
    html_url: 'https://example.invalid/untagged-draft', assets: copy(oldAssets)};
  if (draft && options.currentOwned) {
    draft.target_commitish = current;
    draft.body = `${notes.trim()}\n\n<!-- space-sniffer-publisher:v1 ${tag} ${current} -->`;
    draft.assets = copy(newAssets);
  }
  if (draft && options.changeDraft) options.changeDraft(draft);
  const mutations = [];
  let failUpload = Boolean(options.failUpload);
  const missing = () => { const error = new Error('Not found'); error.status = 404; throw error; };
  const github = {rest: {
    git: {getRef: async () => options.tagTarget ? {data: {object: {type: 'commit', sha: options.tagTarget}}} : missing()},
    repos: {
      getReleaseByTag: async () => { throw new Error('Draft tag lookup must never be used'); },
      listReleases: async () => ({data: draft ? [copy(draft)] : []}),
      getRelease: async ({release_id}) => draft?.id === release_id ? {data: copy(draft)} : missing(),
      compareCommitsWithBasehead: async ({basehead}) => {
        assert.equal(basehead, `${old}...${current}`);
        return {data: {status: 'ahead', base_commit: {sha: old}, merge_base_commit: {sha: old},
          behind_by: 0, ahead_by: 1, files: options.sourceChange ? [{filename: 'src/main.rs', status: 'modified'}] :
            [{filename: '.github/workflows/release.yml', status: 'modified'}, {filename: 'scripts/publish_release.cjs', status: 'added'}]}};
      },
      createRelease: async args => {
        mutations.push('create');
        draft = {...args, id: 5000, html_url: 'https://example.invalid/draft', assets: []};
        return {data: copy(draft)};
      },
      updateRelease: async args => {
        assert.equal(args.release_id, draft.id);
        mutations.push(args.draft ? 'retarget' : 'publish');
        draft = {...draft, ...args};
        return {data: copy(draft)};
      },
      deleteReleaseAsset: async ({asset_id}) => {
        mutations.push(`delete:${asset_id}`);
        draft.assets = draft.assets.filter(existing => existing.id !== asset_id);
      },
      uploadReleaseAsset: async args => {
        mutations.push(`upload:${args.name}`);
        if (failUpload) { failUpload = false; throw new Error('Synthetic upload interruption'); }
        const uploaded = asset(args.name, args.data, 100 + draft.assets.length);
        draft.assets.push(uploaded);
        return {data: copy(uploaded)};
      },
    },
  }};
  return {root, github, context: {repo: {owner: 'siarheisinelnikau', repo: 'space-sniffer'}, sha: current},
    env: {RELEASE_TAG: tag, RELEASE_SHA: current}, core: {setOutput() {}, notice() {}},
    mutations, getDraft: () => draft, newAssets};
}

test('resumes a known untagged draft by numeric ID and verifies every rebuilt asset', async () => {
  const state = fixture();
  await publish(state);
  assert.equal(state.getDraft().draft, false);
  assert.equal(state.getDraft().target_commitish, state.context.sha);
  assert.equal(state.mutations[0], 'retarget');
  assert.equal(state.mutations.at(-1), 'publish');
  for (const expected of state.newAssets) {
    const actual = state.getDraft().assets.find(item => item.name === expected.name);
    assert.equal(actual.digest, expected.digest);
  }
});

test('a completed current owned draft is published without replacing assets', async () => {
  const state = fixture({currentOwned: true});
  await publish(state);
  assert.deepEqual(state.mutations, ['publish']);
});

test('a failed upload remains a draft and the next run safely resumes by ID', async () => {
  const state = fixture({failUpload: true});
  await assert.rejects(() => publish(state), /Synthetic upload interruption/);
  assert.equal(state.getDraft().draft, true);
  assert.ok(!state.mutations.includes('publish'));
  await publish(state);
  assert.equal(state.getDraft().draft, false);
});

test('creates and publishes a new complete draft when no release exists', async () => {
  const state = fixture({none: true});
  await publish(state);
  assert.equal(state.mutations[0], 'create');
  assert.equal(state.mutations.at(-1), 'publish');
});

for (const [name, options, expression] of [
  ['published release', {changeDraft: draft => { draft.draft = false; }}, /published release/],
  ['different draft ID', {changeDraft: draft => { draft.id = 999; }}, /unrelated draft/],
  ['different draft tag', {changeDraft: draft => { draft.tag_name = 'v9.9.9'; }}, /draft tag or identity/],
  ['different draft target', {changeDraft: draft => { draft.target_commitish = 'c'.repeat(40); }}, /target no longer matches/],
  ['changed draft asset', {changeDraft: draft => { draft.assets[0].digest = 'sha256:' + '0'.repeat(64); }}, /draft asset differs/],
  ['application source changes', {sourceChange: true}, /Only workflow and publisher changes/],
  ['existing tag for another commit', {tagTarget: 'c'.repeat(40)}, /tags are never moved/],
]) {
  test(`refuses ${name} before any mutation`, async () => {
    const state = fixture(options);
    await assert.rejects(() => publish(state), expression);
    assert.deepEqual(state.mutations, []);
  });
}
