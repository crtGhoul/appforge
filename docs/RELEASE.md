# AppMaka release checklist

How a version gets from a tag to the user's PC. Follow in order; the
updater manifest step is where v0.9.1 broke (double-base64-encoded
signatures → "Invalid encoding in minisign data" on every NSIS-path
update), so it has hard assertions now.

## 1. Version bump

`package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`.
Never hand-edit `src-tauri/Cargo.lock` — cargo updates it on build.

## 2. Checks (all green before tagging)

- `npx tsc --noEmit` and `npm run build`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings` on Linux AND
  `x86_64-pc-windows-msvc` (needs the AR/RC shims — see AGENTS.md)
- Xvfb smoke per the release's test plan; screenshot new UI at 1440×900

## 3. Tag, push, wait for CI

Commit (explicit paths only — never `git add -A`), tag `vX.Y.Z`, push.
Wait for BOTH workflows (windows-build, linux-build) green.

## 4. Download the CI artifacts

Both platforms' installers AND every `.sig` file into one directory, e.g.
`./release-assets/`. The `.sig` files are what the next step signs with —
they are produced by `tauri build` with `TAURI_SIGNING_PRIVATE_KEY`.

## 5. Create the GitHub release (FULL release, never prerelease)

The updater endpoint (`/releases/latest/download/latest.json`) 404s on
prereleases, so the release must be full (`prerelease: false`).

Upload all 10 assets:
`AppMaka_<v>_x64-setup.exe` (+`.sig`), `AppMaka_<v>_x64_en-US.msi`
(+`.sig`), `AppMaka_<v>_amd64.AppImage` (+`.sig`),
`AppMaka_<v>_amd64.deb` (+`.sig`), `AppMaka-<v>-1.x86_64.rpm` (+`.sig`).

## 6. Generate + verify the updater manifest

```sh
node scripts/make-latest-json.mjs <v> ./release-assets/ "<one-paragraph notes>"
```

The script embeds each `.sig` file's content VERBATIM (it is already the
single base64 encoding of the minisign signature — never encode it again)
and then asserts, aborting loudly before writing anything:

1. the signature's length === the `.sig` file's length (a re-encode
   changes 436 chars to 584 — this is exactly the v0.9.1 failure);
2. one base64 decode yields text starting with `"untrusted comment:"`;
3. the trusted comment names the expected artifact file and version.

Upload the resulting `latest.json` to the release, then curl-verify:

```sh
curl -sL https://github.com/crtGhoul/appforge/releases/latest/download/latest.json
```

Confirm it serves the new version and each signature is 436 chars that
single-decodes to `untrusted comment: ...`.

## 7. Report

Release URL + what was tested vs logic-only, under
`~/workspace/appforge-smoke/vXYZ/`.
