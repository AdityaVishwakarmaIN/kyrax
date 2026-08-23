# Releasing & CI/CD guide for `kyrax`

This document explains how this repository is set up to publish the **`kyrax`**
package to [PyPI](https://pypi.org/project/kyrax/), and exactly what to do to
ship a new version, run tests without publishing, and troubleshoot.

---

## 1. What this project is

- **PyPI package name:** `kyrax` (`pip install kyrax`)
- **Language:** Rust core (in `src/`) exposed to Python via [PyO3](https://pyo3.rs) + [maturin](https://www.maturin.rs)
- **Python package source:** `python/kyrax/`
- **Build backend:** `maturin` (declared in `pyproject.toml`)
- **Wheels are `abi3`** (`abi3-py310`): one wheel works on CPython 3.10 and every
  newer version, so we don't build a separate wheel per Python minor.

### The version number lives in ONE place

The single source of truth for the version is **`Cargo.toml`**:

```toml
[package]
version = "1.0.4"
```

`pyproject.toml` declares `dynamic = ["version"]`, so maturin copies the version
from `Cargo.toml` automatically. **Do not** hard-code a version in
`pyproject.toml`.

---

## 2. How this repo was set up (one-time, already done)

You do not need to repeat any of this — it's recorded here so the setup is
understood and reproducible.

1. **Metadata** in `pyproject.toml` / `Cargo.toml`: name `kyrax`, MIT license,
   `requires-python >=3.10`, README wired up as the PyPI long description
   (`readme = "README.md"`), and project URLs pointing at the GitHub repo.
2. **Isolated git repository:** this folder (`kyrax/`) is its own git repo,
   independent of any parent folder. Its remote `origin` is the private GitHub
   repo **https://github.com/AdityaVishwakarmaIN/kyrax**.
3. **PyPI credentials:** a PyPI API token is stored as the GitHub Actions
   **secret `PYPI_API_TOKEN`** (Repo → Settings → Secrets and variables →
   Actions). The release workflow reads it as `MATURIN_PYPI_TOKEN`. The token is
   never committed to the repo.
4. **Workflows** live in `.github/workflows/` (see next section).

### Rotating / replacing the PyPI token

If the token is ever lost or needs rotating, create a new one at
<https://pypi.org/manage/account/token/> (scope it to the `kyrax` project) and
set it with the GitHub CLI:

```bash
gh secret set PYPI_API_TOKEN
# paste the token when prompted, then press Enter
```

---

## 3. The CI/CD workflows

| Workflow | File | Trigger | What it does |
|----------|------|---------|--------------|
| **CI** | `.github/workflows/CI.yml` | every push to `main` + every PR | Lint, type-check, run the test suite across OSes/Python versions, and verify wheels/sdist build. **Never publishes.** |
| **Release** | `.github/workflows/release.yml` | pushing a `v*` **tag**, or manual "Run workflow" | Builds all platform wheels + sdist, uploads them to **PyPI**, and creates a GitHub Release. |
| **Docs** | `.github/workflows/docs.yml` | manual only | Builds and deploys versioned docs to a `gh-pages` site. Disabled from auto-running until GitHub Pages is set up. |

**Key point:** publishing to PyPI **only** happens when a version tag (`vX.Y.Z`)
is pushed. Ordinary commits and pushes to `main` run tests but never publish.

---

## 4. Commit and push WITHOUT publishing

This is the normal, everyday flow. Pushing commits to `main` runs CI (tests)
but does **not** touch PyPI.

```bash
# make your changes, then:
git add -A
git commit -m "describe your change"
git push
```

That's it. The **CI** workflow runs and gives you pass/fail feedback. Nothing is
published. You can push as many times as you like this way.

---

## 5. Publish a NEW version

Publishing is a deliberate two-part action: **bump the version**, then **push a
matching tag**.

### Step 1 — bump the version in `Cargo.toml`

Edit `Cargo.toml` and increase the version, following
[semantic versioning](https://semver.org):

- **Patch** (`1.0.0` → `1.0.1`): bug fixes, no API change
- **Minor** (`1.0.0` → `1.1.0`): new features, backward compatible
- **Major** (`1.0.0` → `2.0.0`): breaking changes

```toml
[package]
version = "1.0.4"   # was 1.0.3
```

### Step 2 — commit the bump

```bash
git add Cargo.toml
git commit -m "Release 1.0.4"
git push
```

### Step 3 — tag and push the tag

The tag **must** be `v` + the exact version you put in `Cargo.toml`:

```bash
git tag v1.0.4
git push origin v1.0.4
```

Pushing the tag triggers the **Release** workflow, which builds all wheels and
uploads them to PyPI. You can watch it at:
<https://github.com/AdityaVishwakarmaIN/kyrax/actions>

Within a few minutes of the workflow finishing, the new version is live at
<https://pypi.org/project/kyrax/> and installable with `pip install -U kyrax`.

> ⚠️ **The version number must be new every time.** PyPI refuses to overwrite an
> existing version. The workflow uses `--skip-existing`, which means if you
> forget to bump the version, it silently skips the upload and **nothing gets
> published**. If a release "succeeds" but PyPI didn't change, this is almost
> always the cause — bump the version and tag again.

### Manually re-running a release

You can also trigger the Release workflow by hand from the GitHub UI
(Actions → Release → "Run workflow"). This is useful for re-attempting a failed
publish. It still only uploads versions that don't already exist on PyPI.

---

## 6. Building locally (optional sanity check)

You don't need this to publish, but to verify a build on your machine:

```bash
# build a source distribution (what gets uploaded as the .tar.gz)
maturin sdist -o dist

# build a wheel for your current platform and install it into the dev venv
maturin develop --release
```

Requires the Rust toolchain and `maturin` (installed in the project's `.venv`).

---

## 7. Actions minutes & repo visibility

This is a **private** repo. GitHub gives private repos a monthly free budget of
Actions minutes (2000/month on the Free plan), and **macOS runners bill at
10×**, Windows at 2×, Linux at 1×. A full release builds ~20 jobs, so an
occasional tag-triggered release fits comfortably in the budget — which is
exactly why publishing is tied to tags, not to every push.

If you ever want publishing to run automatically on **every push** instead of on
tags, the cheapest way is to make the repo **public** (public repos get
unlimited free Actions minutes). Ask and the trigger can be switched.

---

## 8. Quick reference

```bash
# ---- everyday work (tests only, no publish) ----
git commit -am "..." && git push

# ---- ship a new version ----
# 1. edit Cargo.toml -> bump `version`
# 2. commit + push the bump
git commit -am "Release X.Y.Z" && git push
# 3. tag it (v + the version) and push the tag
git tag vX.Y.Z && git push origin vX.Y.Z

# ---- watch the release ----
gh run watch --exit-status         # or visit the Actions tab

# ---- rotate the PyPI token ----
gh secret set PYPI_API_TOKEN
```
