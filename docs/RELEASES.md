# Release automation

NANDuNX releases are built and published only through GitHub Actions. Pushing
an annotated `vX.Y.Z` tag to the `github` remote starts native Linux and
Windows builds. The publisher creates or updates the GitHub Release only
after both builds succeed, and attaches a combined `SHA256SUMS` manifest.

The tag must exactly match the version in root `Cargo.toml`, `package.json`,
and `src-tauri/tauri.conf.json`. The release build rejects a mismatch.

```bash
git tag -a v0.2.2 -m "NANDuNX v0.2.2"
git push github v0.2.2
```

Do not move or reuse a published version tag. Rerunning a workflow can replace
assets with the same names but cannot change the commit the tag references.

The release contains a Debian/Ubuntu `.deb`, an unsigned Windows x64 NSIS
installer, Linux Web and Docker/TrueNAS archives, and `SHA256SUMS`. GitHub
also supplies source ZIP and tarball downloads for the tag.

GitHub-hosted `ubuntu-22.04` and `windows-2022` runners build the artifacts.
They have no NAND, keysets, or access to user devices. The publishing job uses
only the repository `GITHUB_TOKEN` with `contents: write`; no personal token
or repository secret is needed. Actions must be enabled and repository
workflow permissions must allow creating releases.
