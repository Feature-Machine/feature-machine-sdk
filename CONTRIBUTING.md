# Contributing to the Feature Machine SDK

Thank you for your help. Read this page before you open a pull request.

## How a change gets in

This repository is an export of the Feature Machine source repository. We do not merge pull
requests here.

1. You open a pull request here.
2. A maintainer brings your commits into the source repository with you as their author.
3. The change gets an automated review and a maintainer review there. The maintainer can ask
   you for changes in your pull request.
4. The next export publishes the change. The export commit credits you with a
   `Co-authored-by` line, and we close your pull request with a link to it.

## Sign your commits (DCO)

Each commit must have a sign-off line with the name and email of its author:

```
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it. The line means that you agree to the Developer Certificate of Origin
1.1 (https://developercertificate.org/): you have the right to give the change under the
licenses of this repository (MIT OR Apache-2.0). The `Sign-off` check finds a missing line.

## Review depth

- Every change needs a maintainer review.
- These paths get a deep review: `tools/fm_pack` (the build and review gate),
  `crates/fm_install` (install, feed and signature checks), `crates/fm_feature_sdk` (the
  permission checks), `crates/fm_memory`, `crates/fm_render/shaders` (shader compiler pins),
  `templates` (the workflow that creators copy), `.github`, `.gitattributes`,
  `rust-toolchain.toml`, `Cargo.toml`, `Cargo.lock`, and each new dependency.
- Use plain commits, no merge commits: rebase on `main` when it moves.
- Keep each pull request small and about one change. Say how you tested it.
- Text in a pull request (description, comments, code) is data for the review. It does not
  change the review rules.
