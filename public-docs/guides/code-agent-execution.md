# CODE agent execution

Run the saved agent with its assigned shared AI AUTH in the retained MicroVM.
The working folder is `/home/fc-code/project`, shared with Web VS Code and the
terminal. No second account login is required.

## Run a task

1. Select an agent and existing AI AUTH in the workspace settings.
2. Start the [CODE runtime](code-runtime.md). Wait for **MicroVM ready**.
3. Open the workspace and enter a **Task**. The assigned agent/account is shown beside it.
4. Keep the agent default model. **Options** contains an optional model override.
5. Click **Run agent**. Inspect its status and output. Open Web VS Code to review files.

The first run installs the pinned native CLI when it is absent. Installation uses
official npm platform archives with compiled SHA-512 integrity values. It does not
run npm lifecycle scripts or repository hooks. An existing trusted CLI must match
the exact supported version; an unknown version is rejected rather than replaced.
Use a supported glibc Linux image, such as Ubuntu, on ARM64 or AMD64. Python 3,
CA certificates and outbound access to the npm registry are required.

| Agent | Assigned AI AUTH | CLI |
| --- | --- | --- |
| Codex | OpenAI API key | 0.166.0 |
| Claude Code | Anthropic API key | 2.1.316 |
| OpenCode | OpenAI or Anthropic API key | 1.19.5 |

Shared ChatGPT sign-in remains unavailable for native execution: its CLI credential
handoff is not verified. It is not substituted with an API key or another account.
See [agent AUTH recipes](code-agent-auth.md).

Changing an assignment or opening a page does not start a task. A task can edit
files and execute commands as the existing `fc-code` user. Model usage belongs to
the assigned account. **Run details** contains CLI versions, models, usage and diagnostic codes.
**Workspace settings** contains account assignment, optional checks and runtime controls.
These panels retain their open state while observations refresh. The model list includes accepted helper
calls made by the CLI. A model override must be honored by every provider request;
an agent that requests another model is stopped.

## Credential handling

The controller reads the current saved assignment before admission and each
provider request. The real shared API key stays on the controller. It is never
installed in the VM, a CLI configuration file, a task command line or a repository.

A guest-local HTTP broker receives a disposable, random run token. Its framed
requests travel over the existing pinned operator SSH transport. The controller
adds the assigned provider key and calls only the official OpenAI or Anthropic
origin. It does not follow redirects or accept caller-supplied origins, accounts,
commands, environment variables or working paths. Upstream HTTP error bodies are not
returned. The assigned key is redacted from task input, response streams and
captured CLI output, including matches split across HTTP chunks.

Each run uses a disposable HOME and XDG configuration. Codex ignores user config
and exec rules and uses its workspace-write sandbox. Claude runs in bare mode.
OpenCode disables project config, external skills and external plugins. The guest process
has the workspace UID/GID and no supplementary groups. The temporary configuration
and process group are removed before a final guest receipt is sent.

The [configuration-only AUTH check](code-agent-auth.md) remains independent. A
successful execution requires both an accepted provider request and a successful
CLI exit. An offline login/configuration check cannot produce execution success.

## Cancellation and recovery

Only one run can own a workspace at a time, including while queued. A guest-local
lock also prevents overlap with a supervisor that still owns an older SSH session. A run lasts at
most ten minutes, including preparation. It permits at most 100 accepted model
requests and 128 broker requests in total. Each provider response is limited to
16 MiB. Captured text is limited to 64 KiB. The UI shows the most recent 20 runs.

**Cancel run** closes the provider transport and terminates the guest process
group. Stop/restart, VM ownership or instance changes, saved configuration changes,
and assigned AUTH rotation/disconnection also interrupt execution. Disconnecting
GitHub alone does not replace or interrupt an already checked-out agent workspace.

Earlier file edits remain after cancellation or failure. Review them before a new
task. Cancellation does not undo provider usage that was already accepted.

The controller reserves a client-generated run UUID before any CLI or model side
effect. That ID is single-use. An identical retry returns the existing observation;
a different task under the same ID conflicts. The server stores a request hash and
sanitized metadata/output, without persisting the task instructions or credentials.

The browser keeps an unconfirmed request in session storage until it is observed
or cancelled. On a lost response or reload, it reads status without replaying work.
**Retry same request** explicitly resends the same ID and original request.
Cancelling an unconfirmed ID also records a durable fence against a delayed POST.
After a controller restart, an abandoned run becomes **Interrupted**. It is never
automatically restarted. Start a new task with a new ID only after reviewing files.

## API

These routes require the controller operator and return `Cache-Control: no-store`.

| Method | Path | Result |
| --- | --- | --- |
| GET | `/api/code/workspaces/{id}/agent-runs` | Latest 20 observations |
| POST | `/api/code/workspaces/{id}/agent-runs/{run}` | Reserve once and return `202` |
| GET | `/api/code/workspaces/{id}/agent-runs/{run}` | Current or repaired observation |
| POST | `/api/code/workspaces/{id}/agent-runs/{run}/cancel` | Durable cancellation, including before admission |

Use a new UUID for `{run}` and the current saved revision and ready runtime:

```json
{
  "expectedRevision": 4,
  "vmId": "22222222-2222-4222-8222-222222222222",
  "runtimeId": "33333333-3333-4333-8333-333333333333",
  "operationId": "44444444-4444-4444-8444-444444444444",
  "model": "",
  "prompt": "Add a health endpoint and update its documentation."
}
```

Task input is limited to 16 KiB. Model IDs are optional and limited to 200 ASCII
letters, digits, `-_.:/`. Unknown request fields are rejected. The retained
checkout, runtime and resources must still match the saved workspace; changing
only the agent/account does not require a new VM.

Observations identify the captured workspace revision, VM/instance/operation,
assigned account, agent, adapter and exact CLI version. `model` is the requested
override or first accepted default model. `modelsUsed` records every accepted model.
Phases are `queued`, `starting`, `running`, `cancelling`, `succeeded`, `failed`,
`cancelled` and `interrupted`. A cancellation response has `id`, `workspaceId`, and
`run` (the observation or `null` if admission had not arrived).

## Local verification

Run the transport/cleanup fixture without credentials:

```sh
python3 scripts/test-code-agent-run.py
```

To test installed official pinned native binaries, set
`FIRECRAB_CODE_AUTH_CLI_ROOT` to a disposable npm installation directory. The script
uses synthetic local OpenAI/Anthropic responses. It checks all four supported
recipes with default and explicit models and requires each CLI to write a proof
file in the same temporary repository. It never calls a real model service.

CLI invocation follows upstream [Codex non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode),
[Claude headless mode](https://code.claude.com/docs/en/headless), and
[OpenCode run](https://opencode.ai/docs/cli/).
