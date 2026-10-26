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

Without `gitRef`, the existing current-source snapshot behavior remains available. Selected skills and references use permitted source-relative regular files. Absolute paths, traversal, links, private account/configuration directories and credential file names remain denied. Skills must select `SKILL.md`. The limit is eight skills, sixteen references, 32 KiB per context file and 64 KiB of combined context. Reference files are supplied to the model as untrusted data. All connected provider credentials are redacted before provider requests.

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

Before Test starts, it records the successful Debug task/attempt IDs, result, tests, changed paths and artifact references in `dependencyResults`. Debug changes are copied into Test's own isolated workspace. Conflicting dependency edits fail safely. The authorized original source stays unchanged until selected-file apply.

## Tests and artifacts

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
