# Installing fal-os

You need an Apple Silicon Mac on macOS 26 or newer, a fal account and a Claude account.

1. Download `fal-os-<version>-arm64.dmg` from the internal channel.
2. Open it and drag **fal-os** to **Applications**.
3. This build isn't notarized yet, so macOS blocks the first open. Do one of these, once per download:
   - Terminal: `xattr -dr com.apple.quarantine /Applications/fal-os.app`
   - Or open fal-os, click **Done** on the warning, then go to System Settings → Privacy & Security and click **Open Anyway**.
4. Open fal-os. Setup has two steps:
   1. **Paste your fal key.** [open fal.ai/dashboard/keys] opens the keys page. Create a key with the AGENT preset, copy it, and click [paste].
   2. **Sign in to Claude.** Click **sign in**. If Claude Code isn't installed, fal-os installs it first. Then finish the sign-in in your browser.

   ffmpeg and the motion tools (~0.9 GB) download in the background. Press **start →** when setup says ready.

That's all. fal-os doesn't ask for Keychain, folder, clipboard, Accessibility or Screen Recording access. You may see these macOS prompts later, each only when it's needed:
- **Notifications:** the first time a run finishes or needs you while fal-os is in the background. fal-os shows a one-line note right before.
- **"Background Items Added":** only if you turn on "open at login" (setup → [+] more).
- **Folder access** (Desktop, Documents, Downloads, iCloud Drive or a USB drive): only if you paste a file you copied in Finder from one of those folders. A note appears right before. Dragging files in or picking them in a dialog doesn't ask.
- **Keychain:** only if an earlier fal-os build saved your key in the Keychain and you click [use that key]. macOS asks once; click **Allow**.

Problems? Send `~/.fal-os/logs/host.log` and `~/.fal-os/logs/setup.log`.
