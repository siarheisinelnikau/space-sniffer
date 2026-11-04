const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

function releaseAssets(tag) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) throw new Error('Expected a stable release tag.');
  const windows = `SpaceSniffer-${tag}-windows-amd64.exe`;
  const ubuntu = `SpaceSniffer-${tag}-ubuntu-amd64.deb`;
  return [
    {name: windows},
    {name: ubuntu},
    {name: `SpaceSniffer-${tag}.exe`, aliasOf: windows, label: 'Compatibility: Windows amd64 (older updater)'},
    {name: `space-sniffer_${tag.slice(1)}_amd64.deb`, aliasOf: ubuntu, label: 'Compatibility: Ubuntu amd64 (older updater)'},
    {name: 'SHA256SUMS.txt'},
  ];
}

function stageRelease(directory, tag) {
  const assets = releaseAssets(tag);
  for (const asset of assets.filter(a => a.aliasOf)) {
    const source = fs.readFileSync(path.join(directory, asset.aliasOf));
    const destination = path.join(directory, asset.name);
    if (fs.existsSync(destination)) {
      if (!fs.readFileSync(destination).equals(source)) throw new Error('Compatibility asset differs from its canonical file.');
    } else {
      fs.writeFileSync(destination, source, {flag: 'wx'});
    }
  }
  const checksums = assets.slice(0, -1).map(asset => {
    const data = fs.readFileSync(path.join(directory, asset.name));
    if (!data.length) throw new Error('Empty release asset.');
    return `${crypto.createHash('sha256').update(data).digest('hex')}  ${asset.name}\n`;
  }).join('');
  fs.writeFileSync(path.join(directory, 'SHA256SUMS.txt'), checksums);
}
module.exports = {releaseAssets, stageRelease};
if (require.main === module) stageRelease(path.resolve('release'), process.env.RELEASE_TAG);
