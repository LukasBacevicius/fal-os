# Installing fal-os

You need an Apple Silicon Mac on macOS 26 or newer, a fal account and a Claude account.

1. Download `fal-os-<version>-arm64.dmg` from the internal channel.
2. Open it and drag **fal-os** to **Applications**.
3. This build isn't notarized yet, so run this once in Terminal: `xattr -dr com.apple.quarantine /Applications/fal-os.app`
4. Open fal-os. If macOS asks about the "ai.fal.os" Keychain item, click **Always Allow**.
5. Follow the setup screen: paste a fal key (it opens the keys page for you), click install and sign in for Claude Code, and press **start** when it says ready.

Problems? Send `~/.fal-os/logs/host.log` and `~/.fal-os/logs/setup.log`.
