# The cask in Villain-Studios/homebrew-tap, written from this file by
# .github/workflows/tap.yml when a release is published: it fills in the
# version and the .dmg's SHA-256 (docs/releasing.md). Change the cask here,
# not in the tap, which is overwritten.
cask "villain-layer" do
  version "@VERSION@"
  sha256 "@SHA256@"

  url "https://github.com/Villain-Studios/villain-layer/releases/download/v#{version}/Villain-Layer-#{version}-macos-universal.dmg"
  name "Villain Layer"
  desc "Agent development environment"
  homepage "https://github.com/Villain-Studios/villain-layer"

  livecheck do
    url :url
    strategy :github_latest
  end

  # Only macOS 27 has been tried. Lower this once an older one has been.
  depends_on macos: :golden_gate

  app "Villain Layer.app"

  # Replacing the bundle under a running app kills it, and every agent
  # running in it. `uninstall quit:` would not help: quitting ends them too.
  # This runs before an upgrade or uninstall touches anything, so refusing
  # here leaves the installed app as it was.
  uninstall_preflight_steps do
    run "/bin/sh",
        args: [
          "-c",
          "if /usr/bin/pgrep -f 'Villain Layer[.]app/Contents/MacOS/villain-layer' >/dev/null; then " \
          "echo 'Villain Layer is running. Quit it first, from a terminal outside it: replacing " \
          "the app while it runs kills it, and every agent running in it.' >&2; exit 1; fi",
        ]
  end

  # Not ~/.villain-worktrees: the task folders hold work that may not be
  # pushed. Not the keychain item, which brew cannot remove.
  zap trash: [
    "~/Library/Application Support/eu.codevillain.villain-layer",
    "~/Library/Caches/eu.codevillain.villain-layer",
    "~/Library/Logs/villain-layer",
    "~/Library/Preferences/eu.codevillain.villain-layer.plist",
    "~/Library/WebKit/eu.codevillain.villain-layer",
  ]

  caveats <<~EOS
    Villain Layer is not notarised by Apple yet, so macOS stops it the first
    time with "Apple could not verify…". Choose Done, then System Settings →
    Privacy & Security → Open Anyway. macOS also asks for keychain access
    again after each upgrade.

    Its tokens stay in the keychain (item eu.codevillain.villain-layer)
    after `brew uninstall --zap`. Delete them in Keychain Access.
  EOS
end
