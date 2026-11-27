DATfix {{VERSION}} repairs the `.DAT` file a Canon camera leaves on the card when a recording is
cut off, and turns it into a playable MP4. Free and open source (MIT).

## Download

| You have | Download |
|---|---|
| Mac (Apple silicon or Intel) | `DATfix_{{VERSION}}_macos_universal.dmg` |
| Windows 10 or 11 (64-bit) | `DATfix_{{VERSION}}_windows_x64-setup.exe` |
| Command line, Mac (Apple silicon) | `datfix-cli-{{VERSION}}-aarch64-apple-darwin.tar.gz` |
| Command line, Mac (Intel) | `datfix-cli-{{VERSION}}-x86_64-apple-darwin.tar.gz` |
| Command line, Windows | `datfix-cli-{{VERSION}}-x86_64-pc-windows-msvc.zip` |

`SHA256SUMS.txt` lists a checksum for each file. Check a download with
`shasum -a 256 -c SHA256SUMS.txt --ignore-missing` (macOS) or
`Get-FileHash <file> -Algorithm SHA256` (Windows).

The Mac app is signed with an Apple Developer ID and notarized by Apple, so it opens normally.
The Windows installers are not code-signed yet: SmartScreen may warn about an unknown publisher.
Choose **More info**, then **Run anyway**.

{{CAMERAS}}

See the [README](https://github.com/lelanddutcher/dat-file-repair/blob/{{TAG}}/README.md#supported-cameras)
for what each status means, and the
[user guide](https://github.com/lelanddutcher/dat-file-repair/blob/{{TAG}}/docs/user/guide.md) for
step-by-step use.

## Changes

<!-- Write the notable changes here before publishing this draft. -->
