# Official project skills — 2026-11-02

Issue: [#550](https://github.com/SteelCrab/firecrab/issues/550). Delivery:
[Draft PR #562](https://github.com/SteelCrab/firecrab/pull/562), targeting
`release/v0.5.0`. Work remains on `feat/550-ai-canvas`. The PR is unmerged and
has no auto-merge. This is partial implementation evidence, not issue acceptance.

## Baseline CI result

Exact preceding head `4a0408ef43ee6fb54a858d9e1f1e2ddc62479fb7` completed
[CI](https://github.com/SteelCrab/firecrab/actions/runs/41406449115) successfully.
[Release Build](https://github.com/SteelCrab/firecrab/actions/runs/41406449193)
completed successfully on attempt 2. Attempt 1 passed 1753 API tests and failed the
service stop regression `replicas_inherit_the_group_before_boot_and_redeploy_with_the_same_selection`
with `deployment never reached Stopped`. The unchanged commit passed when the failed
job was rerun. No assertion, timeout, memory reserve or production stop behavior
was weakened. The first failure remains recorded rather than claimed as a source fix.
A local macOS attempt hit the fixture's Linux host-memory check before that stop;
it is not counted as a passing reproduction.

The external [Codecov patch check](https://app.codecov.io/gh/SteelCrab/firecrab/pull/562)
is separate. Its threshold was not changed. Earlier head results do not qualify the
new skill implementation until its own CI finishes. The first skill delivery
`ab6e85e963ae1f90b974fd70001459b5b47952b3` subsequently passed both
[CI](https://github.com/SteelCrab/firecrab/actions/runs/41415570815) and
[Release Build](https://github.com/SteelCrab/firecrab/actions/runs/41415570835).
Its external Codecov patch check still failed. Follow-up changes require their own checks.

## Implemented behavior

- Explicit official-source refresh with commit-pinned project catalogs, source
  filters, search and separate plugin skill bundle/individual skill labels.
- Fixed OpenAI/Anthropic repositories, bounded HTTPS responses, no redirects,
  complete Git tree checks and independent pinned Git blob verification. Remote
  publisher entries in an official marketplace cannot redirect this adapter.
- Complete package bytes, ancestor licenses/notices and fixed file-operation IDs
  retained in schema 9 before any guest write. Actual installation reuses the
  existing versioned retained-volume file transport and its write receipts.
- Per-file progress checkpoints, explicit resume after interruption, accepted UUID
  retries without source re-download and no selectable partial installation.
- Project-shared installed revisions and caller-private Canvas/Personal Page
  selections. Model changes retain incompatible selections as inactive. Request
  checkpoints fix the compatible skill IDs before task dispatch.
- Existing canonical Sandbox tasks retain exact selected instruction bytes and
  resource-history versions. A read-only selected-resource tool cannot read an
  unselected installation or an arbitrary project path. The current provider and
  execution permissions are still checked before each model turn.
- Removal disables future admissions while retaining accepted tasks, historical
  package files and immutable resource versions. It does not claim physical purge.
- Private installation-request checkpoints survive ambiguous responses. A fresh,
  definite pre-admission rejection releases the draft only after an absent-record
  proof. Later retries after an unknown result retain the original UUID. Explicit
  local draft discard leaves project installations and in-flight work retained.
- Fresh task admission rejections can release their private pending selection only
  after the exact task is proven absent. Earlier unknown requests retain their
  original task/snapshot IDs and payload on later rejections.
- Two source-download slots and total catalog/package deadlines bound public-source
  requests. Cached completed retries and frozen-package recovery bypass these slots.
  Installation document quotas count stored bytes, including multibyte metadata.

The [operator/API/recovery guide](../guides/project-official-skills.md) describes
bounds and semantics. Plugin apps, MCP servers, connectors, hooks and dependency
installers are not activated. The adapter loads selected guidance through the
existing Sandbox model protocols; it does not install into a user's Codex/Claude
application. Script execution still requires explicit project shell capability.

## Local and source verification

- Selected API/API-types regressions: **136 passed, 0 failed**. One explicit network
  QA test is ignored in the offline selection and was run separately below.
- API/API-types all-target Clippy with warnings denied: passed.
- Agent request/reconnect/installation Node regressions: **18 passed, 0 failed**.
- Frontend type/production build and lint passed. Existing unrelated warnings remain.
- Documentation links, product versions, formatting and diff whitespace passed.
- Actual official-source network QA: **1 passed, 0 failed**. OpenAI
  `plugins/superpowers/skills/brainstorming`, commit
  `0722921d5542fc593105c27bd52630babd8b8c2a`: **10 files** verified, including
  retained license data. Anthropic `skills/brand-guidelines`, commit
  `ca45fc496a3436251f191354091d557da31c15b5`: **3 files** verified. No downloaded
  package code, hook, dependency installer, model or workload VM executed.
- Storage tests cover original file IDs after reopen, corruption rejection,
  schema 8 migration, prior file/view preservation, removal while stopped,
  immutable accepted resource reads, cross-project/reader denial and quota/owner fences.
- Two hosted browser regressions cover explicit discovery, bundle filters,
  installation/selection, reload/model change, lost acknowledgements and removal.
  The follow-up also checks recovery from a fresh task rejected for a removed skill.
  CI retains their screenshots as an artifact for inspection. The first skill head
  passed **145 frontend tests**; these results do not validate a later head.

The local terminal returned `Unsupported terminal: this terminal cannot display graphics`
when a dedicated verification browser was requested. The in-app browser control
skill file is also absent from the installed bundle. No standalone local headless
browser or unrelated user tab was used. Hosted UI verification and screenshots are
required on each pushed head; their result must be checked separately. The first
skill head produced [hosted screenshots](https://github.com/SteelCrab/firecrab/actions/runs/41415570815/artifacts/13127906180).
They display a synthetic controller and do not prove real workload/provider use.

## Remaining acceptance

Phase 6 remains open. Full acceptance still requires actual installation in a
verified workload VM, authorized real ChatGPT/Claude use, service/VM restart,
removal/model-change behavior and interruption recovery with live evidence. The
current test Pipe AI connection is unavailable. Download/hash tests, SQLite tests
and synthetic browser controllers do not establish that workload acceptance.

The broader native browser/editor lifecycle, multi-user/production-user checks,
automated matched backups and genuine cross-host restoration remain #550 gates.
No production migration, deployment, tag, release publication, integration-branch
merge or PR merge was performed.
