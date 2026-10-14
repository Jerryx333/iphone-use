#!/bin/bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
# DDI_HARDWARE_MODEL：手机的 hardwareModel（例如 D73AP，见
# `xcrun devicectl device info details` 的 hardwareProperties），换机型时设置；
# 不设时沿用 iPhone 14 Pro（ApChipID 0x8120、ApBoardID 0x0C）。
/usr/bin/python3 -I - "$repo" "${DDI_HARDWARE_MODEL:-}" <<'PY'
import hashlib, pathlib, plistlib, shutil, sys
source = pathlib.Path('/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore')
target = pathlib.Path(sys.argv[1]) / 'spikes/self-runner-app/Resources/DDI'
with (source / 'BuildManifest.plist').open('rb') as f:
    manifest = plistlib.load(f)
def number(v):
    return int(v, 16) if isinstance(v, str) else v
model = sys.argv[2].strip().lower()
def matches(i):
    if model:
        return i.get('Info', {}).get('DeviceClass', '').lower() == model
    return number(i.get('ApBoardID')) == 0x0C and number(i.get('ApChipID')) == 0x8120
identities = [i for i in manifest['BuildIdentities'] if matches(i) and 'PersonalizedDMG' in i['Manifest'] and 'LoadableTrustCache' in i['Manifest']]
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
