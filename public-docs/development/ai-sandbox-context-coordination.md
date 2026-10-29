# Sandbox project context, coordination and retained results

These contracts extend the existing Sandbox task engine. They do not introduce a second scheduler or an autonomous-agent framework.

## Project context

Project settings accept the following optional fields:

```json
{
  "context": {
    "gitRef": "refs/heads/main",
    "selectedSkills": [".agents/skills/review/SKILL.md"],
    "references": ["docs/requirements.md"]
  },
  "environmentId": null
}
```

The source must be the authorized local Git repository root when `gitRef` is supplied. Admission resolves that ref to an immutable local commit and exposes it as `task.gitCommit`. No fetch, remote clone, credential forwarding, original checkout mutation or source branch mutation occurs. Each code attempt initializes its own private Git repository from filtered files.

On the Linux controller, every source-authority path component is opened from `/` with no-follow directory descriptors. Startup-owned paths and saved project sources use canonical paths, including initial platform aliases. Replacing an authorized source's ancestor with a symlink cannot redirect file reads, snapshots, apply or Git access.

The controller never runs Git against original repository metadata. It copies only validated HEAD, files-based refs, packed refs and regular loose/pack/index/reverse-index objects into a private bare repository. Source configuration, hooks, includes, remotes and acceleration files are not used. A regular `objects/maintenance.lock` is advisory: it is neither opened nor copied. A symlink or directory with that name is rejected, as are other unknown object entries. Git runs with a minimal configuration, no inherited credentials/protocols, a 512 MiB address-space limit and a 15-second CPU limit. The private metadata copy is limited to 128 MiB and 32,768 files/directories. Ordinary SHA-1/SHA-256 repositories and registered linked worktrees are supported. Linked worktree metadata must have the same owner and its registered backlink must identify the exact authorized source. Arbitrary external `.git` pointers, metadata symlinks/hardlinks, alternates, partial clones, reftable storage and oversized history require an explicit supported repository. They never trigger remote fetching or unconfined Git fallback. Omitting `gitRef` keeps the existing bounded current-file snapshot behavior.

Without `gitRef`, the existing current-source snapshot behavior remains available. Selected skills and references use permitted source-relative regular files. Absolute paths, traversal, links, private account/configuration directories and credential file names remain denied. Skills must select `SKILL.md`. The limit is eight skills, sixteen references, 32 KiB per context file and 64 KiB of combined context. Reference files are supplied to the model as untrusted data. All connected provider credentials are redacted before provider requests.

A Persistent Workspace is bound to its first admitted source directory and resolved Git base. A later source/ref change cannot silently replace retained code or expose a previous source to a different source grant. Admission requires restoring the original source/base or explicitly selecting a new Environment. Retained code is hydrated before model context and file edits. Later edits and deletions are flushed back before a successful result; package and configuration caches remain retained.

## Dependencies and bounded explicit retries

Task requests can add:

```json
{
  "coordination": {
    "dependencies": ["<existing-debug-task-uuid>"],
    "assignedAgent": "Test",
    "retryLimit": 1
  },
  "environmentRole": "test"
}
```

`assignedAgent` is a user-selected label for work performed through the existing verified server-side model adapter. Model selection still uses the project default or the existing task override. Environment role selects a target machine separately from this label.

Dependencies must already exist in the same project and use the same source directory. The caller must have read access to every dependency. At most eight distinct retry chains can be dependencies. A retry preserves the initial task kind, dependency list and retry budget, so it cannot introduce a dependency cycle or increase the budget.

Retries are **bounded explicit retries**. The controller never automatically replays a failed or interrupted side effect. Submit a new task UUID with `retryOf` set to the latest settled task UUID. The initial policy permits zero to three retries and defaults to three. `retryRoot` identifies the first task. `retryNumber` starts at zero. Every retry retains a separate task and attempt identity with its own output and files.

A dependent follows the latest task in each dependency's retry chain. A successful retry can unblock a dependent that originally referenced the first failed task. Failure with remaining budget waits for an explicit retry. Exhausted failure or confirmed cancellation fails dependents before any model or guest call. Interrupted or uncertain guest work requires explicit reconciliation before retry. Dependencies and the project execution queue have a ten-minute bounded wait. Execution has a separate ten-minute limit.

Only one attempt from a project enters execution at a time. Waiting dependents do not take provider permits. API restart marks active and waiting tasks Interrupted. Restart never dispatches them again. Explicit reconciliation and a new task identity are required to continue.

The controller admits at most sixteen pending attempts. New dependency waiters stop at fifteen pending attempts so one slot remains available for an explicit prerequisite retry. Waiting tasks from another dependency chain do not block a settled prerequisite's retry admission. Project and VM claims still serialize execution after admission.

Before Test starts, it records the successful Debug task/attempt IDs, result, tests, changed paths and artifact references in `dependencyResults`. Debug changes are copied into Test's own isolated workspace. Conflicting dependency edits fail safely. The authorized original source stays unchanged until selected-file apply.

## Tests and artifacts

The `run_argv` native tool exposes the V1 guest contract with literal argv, cwd, stdin and hard limits. Its `env` input is an array of `{name,value}` entries so native strict tool schemas remain closed objects. Duplicate names are rejected. The guest REST contract keeps its typed environment object. Existing `run_command` remains available. Both command types produce actual event sequences and can back test records. Managed Environment service variables are injected by the server.

The `record_test` native tool accepts `{name,event_sequence}`. The sequence must identify an actual completed guest-command event. Pass/fail comes from its exit status. A model assertion or file-tool event cannot create a test result. A task retains at most 64 test records.

The `retain_artifact` native tool accepts `{path}` for a permitted workspace file. It copies bytes into private task/attempt storage and records the task ID, attempt ID, original relative path, byte count and SHA-256. Files containing connected provider credentials are rejected. The limit is sixteen files, 1 MiB per file and 8 MiB total. Required binary outputs can be exported, then removed from the editable workspace before the existing UTF-8 change-review step.

Download an exported artifact with:

```text
GET /api/ai-sandbox/tasks/{task_id}/artifacts/{artifact_id}
```

The same project/source authorization used for task inspection applies to downloads. Exported bytes survive guest cleanup and removal of the original workspace file. Downloads and temporary Environment cleanup verify artifact ownership, byte count and SHA-256. Missing or changed bytes block cleanup. `SavedTask.results_exported` becomes true only after retained result preparation finishes and a terminal checkpoint is saved. Environment management owns VM/network deletion and must check this flag and `artifacts::verify_export` first. Explicit project deletion remains an authorized deletion of retained local results.

Conversation and board summaries use the same durable task records. Summaries include retry identity and test/artifact counts. Task detail provides dependency evidence, tests, failure reasons, changes and artifact metadata.

## Validation boundary

Deterministic tests cover pinned Git snapshots, source preservation, credential/path filtering, Debug-to-Test file handoff, latest-retry dependency resolution, exhausted retry blocking, restart interruption, test-result provenance and immutable artifact retention. Provider fixtures validate the REST task flow and reload behavior. These tests do not establish real provider, real two-agent MicroVM, cross-OS, container-driver or public Tunnel acceptance. Record those runs separately before checking the corresponding issue acceptance items.
