# Continuous Integration & Release Automation

**English** | [Tiếng Việt](./ci-workflow.vi.md)
This document outlines the CI/CD pipeline, automated release mechanism, and upstream version polling architecture configured in `.github/workflows/build.yml`.

---

## 1. Pipeline Architecture & Triggers

The build and release automation is defined in `.github/workflows/build.yml` and is driven by four triggers:

| Trigger | Description | Use Case |
| :--- | :--- | :--- |
| `schedule` (`cron: '0 0 * * *'`) | Runs daily at 00:00 UTC | Automatically polls VNG for newly published Zalo desktop releases. |
| `push` (`branches: [main]`) | Triggers on commit push to `main` | Builds and verifies changes when new commits land in the repository. |
| `pull_request` (`branches: [main]`) | Triggers on opening/updating PRs | Runs the build matrix to validate contributor code without releasing. |
| `workflow_dispatch` | Manual trigger via GitHub Actions UI | Allows maintainers to trigger builds with custom version overrides. |

---

## 2. Upstream Polling Mechanism

Because Zalo is proprietary closed-source software developed by VNG, upstream releases do not trigger GitHub webhooks or Git commits in this repository.

To keep Linux packages synchronized with official releases, CI polls VNG's download servers daily:

1. `scripts/check-versions.js` issues an HTTP GET request to `https://zalo.me/download/zalo-pc?utm=90000`.
2. VNG's server responds with an HTTP 302/301 redirect containing the latest universal macOS DMG filename:
   ```text
   Location: .../ZaloSetup-universal-26.11.10.dmg
   ```
3. The script parses the version tag (e.g. `26.11.10`) and queries the GitHub API to check whether a matching release combination already exists.
4. **Conditional Execution**:
   - If the version has already been built: the workflow sets `build=false` and exits immediately without consuming unnecessary runner minutes.
   - If a new version is detected: the workflow sets `build=true` and proceeds to the build matrix.

---

## 3. Build Matrix (`jobs.build`)

The `build` job compiles and packages AppImages across two CPU architectures:

- **`x86_64`**: Runs on `ubuntu-latest` inside an `ubuntu:22.04` container.
- **`aarch64`**: Runs on `ubuntu-24.04-arm` inside an `ubuntu:22.04` container.

During the job:
1. Native addons in `nativelibs/` are compiled for the target architecture.
2. The official DMG is downloaded and extracted.
3. JavaScript patches from `scripts/patches/` are applied.
4. ZaDark assets are built and bundled.
5. `electron-builder` and `quick-sharun` package the final portable AppImages.
6. The resulting AppImage files are uploaded as CI artifacts.

---

## 4. Release & Nix Sources Automation (`jobs.release`)

When running on `main` (not in a pull request) and `build=true`, the `release` job executes:

1. **Asset Management**: Replaces or cleans up older release assets for the target version tag.
2. **Publish Release**: Generates a GitHub Release with download links for x86_64 and aarch64 AppImages (both Original and ZaDark variants).
3. **Update Nix Flake Sources**:
   - Executes `scripts/update-nix-sources.sh`, which recalculates SHA256 hashes of the newly published AppImages and updates `nix/sources.json`.
   - `github-actions[bot]` commits the updated hashes directly to `main`:
     ```text
     chore(nix): update sources for <version> (<commit-hash>)
     ```
   - This ensures NixOS users consuming the project flake always have matching asset hashes.

---

## 5. Behavior on Contributor Forks

When you fork `zalo-for-linux`, GitHub Actions workflows are included in your fork:

### The Fork Divergence Issue
- If Actions are enabled on your personal fork, the daily cron schedule will execute on your fork's repository.
- `github-actions[bot]` on your fork will commit updated `nix/sources.json` files to your fork's `main` branch.
- This causes your fork's `main` branch to **diverge** from upstream `main`, leading to merge/rebase conflicts when you pull updates.

### Recommended Fork Setup
1. **Disable Workflows on Fork**:
   - Go to your fork repository on GitHub: `Settings` $\rightarrow$ `Actions` $\rightarrow$ `General`.
   - Select **Disable actions**, or under the **Actions** tab, disable the **Build Zalo for Linux** workflow.
2. **Repository Guard Condition**:
   - For contributors opening PRs to improve CI, guard release steps so they only execute on the upstream repository:
     ```yaml
     if: github.repository == 'VN-Linux-Family/zalo-for-linux' && github.ref == 'refs/heads/main'
     ```
