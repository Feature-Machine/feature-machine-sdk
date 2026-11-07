# Security

## Report a vulnerability

Do not open a public issue or pull request for a security problem.

Use the private report form of this repository: **Security**, then **Report a vulnerability**
(GitHub private vulnerability reporting). Only the maintainers see the report.

Include:

- The affected part: the SDK (`crates/fm_feature_sdk`), the build and review tool
  (`tools/fm_pack`), the install and signature checks (`crates/fm_install`), or the creator
  workflow (`.github/workflows/feature-machine.yml`, `templates/`).
- The version or commit.
- The steps to reproduce, and what an attacker can do with it.

We answer in the report thread. When a fix is released, we publish an advisory that credits you,
unless you ask us not to.

## Scope

In scope: code in this repository, and how the Feature Machine build and review treat a feature
that uses it (for example a way to pass the review with a power that the feature does not
declare). A published feature that does something unsafe is in scope too: use the same form.

Out of scope: denial of service by volume.

## Supported versions

The newest release tag gets security fixes. Creators pin a tag; move to the newest tag to get a
fix.
