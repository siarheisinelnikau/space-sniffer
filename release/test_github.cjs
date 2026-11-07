const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { test } = require('node:test');
const release = require('./github.cjs');

const hash = data => 'sha256:' + crypto.createHash('sha256').update(data).digest('hex');
function fixture() {
  const previous = process.cwd(), previousVersion = process.env.RELEASE_VERSION;
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'release-publisher-'));
  process.chdir(directory);
  process.env.RELEASE_VERSION = '1.2.3';
  fs.mkdirSync('release/notes', { recursive: true });
  fs.mkdirSync('.release-stage');
  const config = { repository: 'owner/test', product: 'Test', assets: [{ name: 'Test-v{version}.exe' }] };
  fs.writeFileSync('release/config.json', JSON.stringify(config));
  const notes = '# Test 1.2.3\n' + 'Verified user-facing improvement. '.repeat(8);
  fs.writeFileSync('release/notes/1.2.3.md', notes);
  const assets = [['Test-v1.2.3.exe', Buffer.from('checked fixture')], ['SHA256SUMS.txt', Buffer.from('checksum fixture')]];
  const manifest = { ...config, version: '1.2.3', tag: 'v1.2.3', commit: 'a'.repeat(40), notes: 'release/notes/1.2.3.md',
    notes_sha256: hash(Buffer.from(notes)).slice(7), assets: assets.map(([name, data]) => ({ name, size: data.length, digest: hash(data) })) };
  for (const [name, data] of assets) fs.writeFileSync('.release-stage/' + name, data);
  fs.writeFileSync('.release-stage/_manifest.json', JSON.stringify(manifest));
  const context = { repo: { owner: 'owner', repo: 'test' }, eventName: 'workflow_dispatch',
    ref: 'refs/heads/main', sha: 'a'.repeat(40), runId: 123, payload: { inputs: { publish: 'true' } } };
  const calls = [];
  let draft;
  const notFound = () => { throw Object.assign(new Error('not found'), { status: 404 }); };
  const github = { paginate: async () => [], rest: { git: { getRef: notFound }, repos: {
    listReleases: () => {}, getReleaseByTag: notFound,
    createRelease: async options => { calls.push('create'); draft = { ...options, id: 7, assets: [] }; return { data: draft }; },
    uploadReleaseAsset: async options => { calls.push('upload'); const asset = { name: options.name, size: options.data.length, digest: hash(options.data) };
      draft.assets.push(asset); return { data: asset }; },
    getRelease: async () => ({ data: draft }),
    updateRelease: async options => { calls.push('publish'); return { data: { ...draft, ...options, html_url: 'https://github.com/owner/test/releases/tag/v1.2.3' } }; },
  } } };
  return { github, context, core: { notice() {} }, calls, manifest,
    cleanup() { process.chdir(previous); if (previousVersion === undefined) delete process.env.RELEASE_VERSION; else process.env.RELEASE_VERSION = previousVersion;
      fs.rmSync(directory, { recursive: true, force: true }); } };
}

test('version comparison uses numbers rather than strings', () => {
  assert.equal(release.compare('v1.10.0', 'v1.9.9'), 1);
  assert.throws(() => release.compare('v1.2.3-beta', 'v1.2.3'));
});

test('an automatic tooling check cannot authorize publication', () => {
  const f = fixture();
  try { f.context.eventName = 'push';
    assert.doesNotThrow(() => release.assertContext(f.context, { repository: 'owner/test' }, false));
    assert.throws(() => release.assertContext(f.context, { repository: 'owner/test' }, true), /explicit/); }
  finally { f.cleanup(); }
});

for (const [name, change, expected] of [
  ['changed asset', f => fs.appendFileSync('.release-stage/Test-v1.2.3.exe', 'changed'), /Asset changed/],
  ['changed notes', f => fs.appendFileSync('release/notes/1.2.3.md', 'changed'), /notes changed/],
  ['missing companion', f => { f.manifest.assets = f.manifest.assets.slice(1); fs.unlinkSync('.release-stage/Test-v1.2.3.exe');
    fs.writeFileSync('.release-stage/_manifest.json', JSON.stringify(f.manifest)); }, /unexpected files|missing checksums/],
  ['unexpected file', f => fs.writeFileSync('.release-stage/private-key.pem', 'fixture'), /unexpected files/],
  ['unconfirmed publication', f => { f.context.payload.inputs.publish = 'false'; }, /explicit/],
  ['untrusted branch', f => { f.context.ref = 'refs/heads/feature'; }, /main branch/],
  ['wrong source version', f => { process.env.RELEASE_VERSION = '1.2.4'; }, /another source/],
]) {
  test(name + ' fails before any GitHub mutation', async () => {
    const f = fixture();
    try { change(f); await assert.rejects(release.publish(f), expected); assert.deepEqual(f.calls, []); }
    finally { f.cleanup(); }
  });
}

test('existing release is preserved', async () => {
  const f = fixture();
  try { f.github.rest.repos.getReleaseByTag = async () => ({ data: { draft: false } });
    await assert.rejects(release.publish(f), /already/); assert.deepEqual(f.calls, []); }
  finally { f.cleanup(); }
});

test('downgrade is refused even when newest release is on another branch', async () => {
  const f = fixture();
  try { f.github.paginate = async () => [{ tag_name: 'v1.10.0', draft: false, prerelease: false }];
    await assert.rejects(release.publish(f), /not newer/); assert.deepEqual(f.calls, []); }
  finally { f.cleanup(); }
});

test('authorization failure is not mistaken for a missing release', async () => {
  const f = fixture();
  try { f.github.rest.repos.getReleaseByTag = () => { throw Object.assign(new Error('forbidden'), { status: 403 }); };
    await assert.rejects(release.publish(f), /forbidden/); assert.deepEqual(f.calls, []); }
  finally { f.cleanup(); }
});

test('wrong server digest leaves an unpublished draft', async () => {
  const f = fixture();
  try { f.github.rest.repos.uploadReleaseAsset = async () => { f.calls.push('upload'); return { data: { digest: 'wrong' } }; };
    await assert.rejects(release.publish(f), /verification failed/); assert.deepEqual(f.calls, ['create', 'upload']); }
  finally { f.cleanup(); }
});

test('complete verified candidate is published only after uploads', async () => {
  const f = fixture();
  try { await release.publish(f); assert.deepEqual(f.calls, ['create', 'upload', 'upload', 'publish']); }
  finally { f.cleanup(); }
});

test('annotated version tags must resolve to the checked commit', async () => {
  const f = fixture();
  try { f.github.rest.git.getRef = async () => ({ data: { object: { type: 'tag', sha: 'b'.repeat(40) } } });
    f.github.rest.git.getTag = async () => ({ data: { object: { type: 'commit', sha: 'c'.repeat(40) } } });
    await assert.rejects(release.publish(f), /another commit/); assert.deepEqual(f.calls, []); }
  finally { f.cleanup(); }
});
