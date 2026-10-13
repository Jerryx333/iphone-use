#!/bin/bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
/usr/bin/python3 -I - "$repo" <<'PY'
import hashlib, pathlib, plistlib, shutil, sys
source = pathlib.Path('/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore')
target = pathlib.Path(sys.argv[1]) / 'spikes/self-runner-app/Resources/DDI'
with (source / 'BuildManifest.plist').open('rb') as f:
    manifest = plistlib.load(f)
def number(v):
    return int(v, 16) if isinstance(v, str) else v
identities = [i for i in manifest['BuildIdentities'] if number(i.get('ApBoardID')) == 0x0C and number(i.get('ApChipID')) == 0x8120 and 'PersonalizedDMG' in i['Manifest'] and 'LoadableTrustCache' in i['Manifest']]
if len(identities) != 1:
    raise SystemExit(f'Expected one mounter identity; found {len(identities)}')
components = identities[0]['Manifest']
files = [('BuildManifest.plist', 'BuildManifest.plist')]
files += [(components[k]['Info']['Path'], name) for k, name in [('PersonalizedDMG', 'Image.dmg'), ('LoadableTrustCache', 'Image.dmg.trustcache')]]
resolved = []
for path, name in files:
    src = (source / path).resolve()
    if source.resolve() not in src.parents or not src.is_file():
        raise SystemExit(f'Invalid or missing DDI asset: {path}')
    resolved.append((src, name))
target.mkdir(parents=True, exist_ok=True)
for src, name in resolved:
    dst = target / name
    shutil.copyfile(src, dst)
    data = dst.read_bytes()
    print(f'{name}: {len(data)} bytes sha384={hashlib.sha384(data).hexdigest()}')
PY
