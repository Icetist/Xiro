# Getting Xiro.exe signed for free (SignPath Foundation)

Unsigned, Windows shows "Windows protected your PC" the first time someone runs Xiro.
SignPath Foundation signs open-source projects for free. Everything on the Xiro side is
ready:

- `Xiro.exe` carries its product name and version (`build.rs`).
- [`release.yml`](../.github/workflows/release.yml) builds every release on GitHub, sends
  it to SignPath and publishes the signed exe. Until signing is set up, it publishes
  unsigned releases.
- [`.signpath/artifact-configuration.xml`](../.signpath/artifact-configuration.xml) is
  the signing configuration to paste into SignPath.
- The README has the "Code signing policy" and "Privacy" sections SignPath requires.

What's left needs the project owner's own accounts (about 15 minutes, $0).

## 1. Put Xiro on GitHub (public)

1. Turn on two-factor sign-in for GitHub: **Settings → Password and authentication**.
   SignPath requires it.
2. Create a **public** repository named `xiro` (no README, no license; Xiro has them).
3. In `D:\Xiro`:
   ```
   git add -A
   git commit -m "feat: Xiro 0.2.0"
   git branch -M main
   git remote add origin https://github.com/<you>/xiro.git
   git push -u origin main
   ```
4. Make the first release (unsigned for now). SignPath only signs projects that already
   have a release:
   ```
   git tag v0.2.0
   git push origin v0.2.0
   ```
   The **Actions** tab shows the build. When it finishes, the release appears under
   **Releases**.

## 2. Apply

Go to <https://signpath.org/apply> and fill in the form. Answers you can paste:

- **Project name:** Xiro
- **Repository:** `https://github.com/<you>/xiro`
- **License:** MIT
- **Description:** Xiro turns an iPad or iPhone into a low-latency second monitor for
  Windows. No app on the device: it streams to Safari over the local network or the
  PC's own Wi-Fi Direct network. It's a single portable native Rust program that, after
  asking, adds a firewall rule limited to the local subnet and installs the
  open-source Virtual Display Driver (MIT, already signed by SignPath Foundation for
  that project) so Windows sees a real monitor.
- **Build system:** GitHub Actions (`.github/workflows/release.yml`), triggered by
  version tags.
- **What is signed:** `Xiro.exe`, built from this repository only.
- **Download page:** the repository's Releases page. The README contains the code
  signing policy and the privacy statement.

They review it by email. Approval can take a few days to a few weeks.

## 3. After approval (one time)

SignPath sends an invitation to their web app. There:

1. Install the **SignPath GitHub App** on the `xiro` repository:
   <https://github.com/apps/signpath>
2. In the SignPath project:
   - open **Artifact Configurations → default** and paste
     `.signpath/artifact-configuration.xml`;
   - check that the **Trusted Build System** is "GitHub.com" and that the signing
     policy (usually `release-signing`) requires your approval.
3. Create an API token for your SignPath user (**Profile → API token**).
4. In GitHub, open the repository's **Settings → Secrets and variables → Actions** and
   add:
   - Secret `SIGNPATH_API_TOKEN`: the token.
   - Variable `SIGNPATH_ORGANIZATION_ID`: shown in SignPath's organization settings.
   - Variable `SIGNPATH_PROJECT_SLUG`: e.g. `xiro`.
   - Variable `SIGNPATH_SIGNING_POLICY_SLUG`: e.g. `release-signing`.

## 4. Every release after that

1. Bump `version` in `Cargo.toml`.
2. Commit, then `git tag vX.Y.Z` and `git push origin vX.Y.Z`.
3. SignPath emails you: approve the signing request.
4. The release appears with the signed `Xiro.exe` and its `.sha256`.

The workflow refuses to publish if the tag and `Cargo.toml` disagree, or if the signed
exe doesn't verify.
