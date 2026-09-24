# Releasing

A release is a version tag. Pushing `v0.2.0` runs
[`release.yml`](../.github/workflows/release.yml): `bun run check` on the
tagged commit, then one universal build (Apple silicon and Intel), then a
**draft** GitHub release with `Villain-Layer-0.2.0-macos-universal.dmg`, its
SHA-256, and notes generated from the pull requests since the last one.
Nothing is public until you publish the draft.

## Cutting one

1. **Bump the version** in `src-tauri/Cargo.toml`, and only there. The app's
   About line, the bundle and the MCP server all read it from Cargo, and
   the workflow refuses a tag that does not match it.
2. Run `cd src-tauri && cargo check`, so `Cargo.lock` follows.
3. Commit (*Release 0.2.0.*) through a pull request as usual, and wait for
   `check` to pass on `main`.
4. Tag that commit and push the tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

5. When the workflow is done, open the draft under Releases, read the
   notes, and publish it.

A tag that failed can be deleted and pushed again once the fix is on
`main`: `git push --delete origin v0.2.0`, then tag the new commit.

## Signing and notarisation

Without them, macOS stops a downloaded app with "Apple could not verify
…", and the only way past is System Settings → Privacy & Security → Open
Anyway. Right-click → Open no longer works for that. An unsigned app also
asks for keychain access again after every update, because *Always Allow*
is tied to the exact binary. The workflow signs ad hoc until the secrets
below exist, and the release notes then tell people how to open it.

What it takes:

1. **An Apple Developer Program membership** (99 USD a year). An
   individual membership signs as your legal name: the certificate reads
   *Developer ID Application: <your name> (TEAMID)*, and macOS shows that
   name. Signing as *Code Villain* needs an organisation membership, which
   needs a legal entity with a D-U-N-S number.
2. **A Developer ID Application certificate.** Create it in Xcode (Settings
   → Accounts → Manage Certificates → + → Developer ID Application) or on
   developer.apple.com. Export it from Keychain Access as a `.p12`, with a
   password.
3. **An app-specific password** for notarisation, from account.apple.com →
   Sign-In and Security → App-Specific Passwords.
4. **The repository secrets.** Set them from a terminal with `gh`, which
   asks for each value without echoing it and without leaving it in your
   shell history. The certificate's name and team id are printed by
   `security find-identity -v -p codesigning`, as
   `Developer ID Application: <name> (TEAMID)`.

   ```bash
   R=Villain-Studios/villain-layer
   base64 -i ~/Desktop/developer-id.p12 | gh secret set APPLE_CERTIFICATE -R $R
   gh secret set APPLE_CERTIFICATE_PASSWORD -R $R   # the .p12's export password
   gh secret set APPLE_SIGNING_IDENTITY -R $R       # Developer ID Application: <name> (TEAMID)
   gh secret set APPLE_ID -R $R                     # the membership's Apple Account email
   gh secret set APPLE_PASSWORD -R $R               # the app-specific password
   gh secret set APPLE_TEAM_ID -R $R                # the ten characters in the brackets
   ```

   Then delete the `.p12` file: the secret holds it now.

All six, or none. With some set and not others the workflow stops, rather
than ship an app that is signed but not notarised, which macOS stops just
the same. With all six, the next tag builds signed and notarised, and the
workflow checks the result the way a Mac that downloads it will (`codesign`,
`stapler validate`, `spctl`) before it drafts anything.

Check the first one on a Mac that has never run the app: download it,
open it, and it should start without a warning. Then check the parts that
the hardened runtime, which notarisation requires, could stop: a terminal
pane starts an agent, a token is saved and read back, a notification
arrives, and an agent reaches the app's MCP tools.

## Moving the identifier

The identifier, `eu.codevillain.villain-layer`, names the config folder
and the keychain item. Changing it again means another one-time copy, as
`previous.rs` does for the last change (DISK-4). Without that, every user
opens an empty app.
