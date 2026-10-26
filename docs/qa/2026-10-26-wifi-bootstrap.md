# Official Wi-Fi bootstrap diagnostic: device validation

Date: 2026-10-26 (UTC+08:00). Follow-up to [#313](https://github.com/leeguooooo/iphone-use/issues/313).

## Environment and method

Official v0.23.0 (`ab32b3f`), macOS 27.0.1, Xcode 27.1, iPhone 17 Pro / iOS 27.0.1. Release checksum, Developer ID signature and notarization were checked. The device runner was rebuilt by official setup after the upgrade; no source patch was installed.

Followed the [CoreDevice proxy diagnostic](../guide.md#coredevice-proxy-diagnostic): temporarily disabled only SOCKS on the active Ethernet and Wi-Fi services, kept HTTP/HTTPS enabled, resumed the official runner over USB with the phone unlocked, unplugged, and checked fresh reads and resulting screens. A 180-second timer restored the original SOCKS flags, verified by reading them back.

No Apple device-service refresh, pairing reset, proxy bypass entry, VPN change or route change was performed. The phone's Settings UI reported VPN connected; its configuration was left unchanged.

## Observations

| Local time | Evidence |
| --- | --- |
| 10:07:48 | CoreDevice `wired/connected`; HTTP/HTTPS on and SOCKS off on both services. Wired Settings launch and read passed. |
| 10:09:12 | After operator-confirmed unplugging: CoreDevice `localNetwork/connected`; SOCKS still off. A Settings screen read succeeded. |
| 10:09:50 | Both original SOCKS flags restored; all six proxy flags on. CoreDevice remained `localNetwork/connected`. Wireless Calculator launch and expected application read passed. |
| By 10:10:18 | Snapshot-bound Calculator key 7 changed input `0 → 7`; Clear changed it `7 → 0`. Each resulting value was read back. Native wireless tunnel still connected; control lease released. |

One Settings General locator tap was acknowledged but did not reach its expected screen; the wait timed out and a subsequent read still showed Settings root. It was not replayed or counted as successful navigation. The later Calculator checks used snapshot-bound taps.

## Conclusion and limits

The documented procedure worked in this run without the additional current-user Apple device-service refresh used in the original report. Normal proxy settings were restored before the successful wireless app launch and input checks. Use the official procedure; this report introduces no alternative recovery implementation.

This is a single-environment operational validation, not an isolated proof that SOCKS caused the failure. There was no same-version randomized on/off comparison; the release upgrade and USB runner restart also occurred. Earlier v0.22.4 unplugged failure is historical evidence, not a controlled v0.23.0 baseline. Pure Wi-Fi cold startup and recovery after a device/Mac reboot remain unverified.

Device identifiers, network addresses, account data and credentials are omitted.
