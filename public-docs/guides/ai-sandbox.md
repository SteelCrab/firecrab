# AI Sandbox

AI Sandbox uses accounts connected in [Pipe AI](pipe-ai.md) to select models, converse, edit a project, and run commands inside a MicroVM.
Pipe AI remains the account-management page.
Open **AI → AI Sandbox**, or `/#/ai-sandbox`.
See [Sandbox access](sandbox-access.md) for caller identity, resource grants, and immutable runtime references.

## Start a project

1. Connect an account in Pipe AI.
2. Create a Sandbox project. Select an account and refresh its model list, or enter a model ID.
3. Choose **Verify access**. Enable tool verification for code tasks. Verification consumes provider usage.
4. Save a default model and optional project instructions.
5. For code tasks, set an absolute project directory **on the API host** and select a running user MicroVM.

On macOS and Windows, the API host is managed Debian. A path on the desktop is not automatically a path inside managed Debian.
Conversation mode uses instructions and recent completed conversation. It does not read source files or execute tools.
Select **Code task** to give the model file and guest-command tools.
The composer can override the default model for one attempt without changing the project default.

## Provider contracts

| Account | Discovery | Inference and native tools |
| --- | --- | --- |
| OpenAI API key | `GET https://api.openai.com/v1/models`, `data[].id` | Responses API with native function calls and correlated `function_call_output` |
| ChatGPT sign-in | Same endpoint with OAuth; visible `models[].slug` and `display_name` | Public Responses API; functions grouped in the `sandbox` namespace; required completed SSE response |
| Anthropic API key | `GET https://api.anthropic.com/v1/models`, `data[].id` | `POST https://api.anthropic.com/v1/messages`; `anthropic-version: 2026-10-21`; native `tool_use` / `tool_result` |

A catalogue entry does not grant inference permission. A text probe must complete before a model can be selected.
A separate harmless `sandbox_ready` call, tool result, and final response verify native tool support before code execution.
Proofs are bound to the saved account, authentication method, provider adapter, and model. Credential changes invalidate access. Disconnect blocks new requests without fallback.
Models are discovered or explicitly configured; the UI does not invent a fixed supported-model list.
Discovery reads the first bounded provider page. Enter an exact model ID if a model is absent from that page, then verify it.

OpenAI API-key and Anthropic turns use buffered provider JSON. ChatGPT uses `store: false`, `stream: true`, array input, and complete history over HTTP. It omits unsupported output-token limits and continuation IDs.
The Dashboard, CLI, and MCP receive complete turn results. Incremental browser token delivery is not implemented.
Provider calls use fixed official endpoints, no redirects, and no automatic inference retry.

These contracts follow the official [OpenAI function-calling guide](https://developers.openai.com/api/docs/guides/function-calling), [ChatGPT model catalogue](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference), [ChatGPT preview restrictions](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations), and [Anthropic client-tool protocol](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview).
Claude Code login and subscription tokens are not used.

## Files, commands, and review

Each code attempt captures the project settings and creates a private snapshot with its own Git repository and `ai/<attempt-id>` branch.
It does not change the source repository's branch or index.
The model can list, read, replace, and delete allowed files in that copy.
Paths cannot escape the opened project directory. Symlinks, hard links, devices, Git metadata, common credential files, and dependency/build directories are excluded. The configured private database and sidecars are never copied, even with a custom filename. Files containing a known connected credential are also omitted. Common generated output (`node_modules`, `target`, `dist`, `coverage`, Python caches, npm caches and Cargo registry/git caches) stays out of review; project `.cargo/config.toml` remains available.
Known connected account credentials are redacted from context, results, and activity. Do not put other secrets in source files or task prompts.

`run_command` uses the selected VM's operator SSH key and pinned host key. The VM must be running, have a current network lease and runtime identity, and permit operator SSH.
The Linux API host needs `/usr/bin/git` and `/usr/bin/ssh`. The guest needs a POSIX shell, `tar` with `--exclude` support, `timeout`, `flock` with timed locks, user-management utilities, and direct UID switching with no-new-privileges. Install util-linux `setpriv` in the selected development VM. The injected BusyBox alone does not satisfy this requirement. A read-only prerequisite probe runs before staging; missing helpers fail explicitly.
A task gets a dedicated unprivileged guest user and project directory. Commands receive a clean environment without provider credentials or SSH-agent forwarding. No-new-privileges prevents gaining privileges through setuid executables.
Packages and other generated dependencies remain available between commands within the same attempt.
The selected VM is borrowed exclusively by the Sandbox project. A service-owned/internal VM cannot be selected.

Commands run with that user's normal guest filesystem and network permissions. This is MicroVM isolation from the host, not an additional container or network sandbox inside the VM. Use a dedicated development VM without sensitive guest data.
The API never runs a model-provided host shell command. The root SSH helper only stages files, supervises the owned user, exports changes, and cleans up owned data.

Open a task to inspect its result, native tool arguments, stdout, stderr, exit code, and before/after file contents and modes.
Only **Apply selected files** changes the original source. Finish or reconcile an active project attempt before applying another result. Every selected original content and mode must still match the snapshot before writes begin.
Atomic source writes preserve existing file ownership; new directories and files inherit project ownership.
Imported modes preserve the original source read/write permissions and accept executable-bit changes; they do not add privilege bits.
A source conflict stops apply. An I/O failure attempts to restore earlier writes; this is not a filesystem transaction.
Text changes support apply. Binary changes remain in the private retained workspace and require manual inspection.
Applying files does not make a commit in the user's source repository.

## Limits and recovery

| Limit | Value |
| --- | --- |
| Concurrent attempts per controller / project | 2 / 1 |
| Pending attempts per controller | 16 |
| Attempt deadline / native turns | 10 minutes / 24 |
| Command timeout | 1–120 seconds |
| Command stdout / stderr | 64 KiB each; excess stops the command |
| Project snapshot | 32 MiB, 4,096 files, depth 32 |
| Individual file / file-tool read | 1 MiB / 32 KiB |
| Prompt / project instructions | 32 KiB / 16 KiB |
| Provider response / assistant text | 1 MiB / 64 KiB |
| Provider history / tool-result context | 192 KiB / 16 KiB per stream |
| Recent conversation context | Up to 12 complete pairs within 48 KiB |
| Retained tasks per project / list page | 500 / 50 |

Task lists use an indexed summary; full bounded activity and file contents are fetched only for task detail.
The full bounded activity stays in the task record when model-context output is clipped.
The model receives an explicit clipping notice. Older conversations remain saved and can be paged in the UI or REST API.
A context, output, or time limit is an explicit outcome, not a fabricated successful result.

Tasks move through **Waiting → Preparing → In progress → Result → Applied**.
Failure and confirmed cancellation retain the attempt identity, received activity, and reviewable changes. An interrupted SSH request may not provide partial stdout/stderr; missing output is not reported as a completed command.
Stop requests interrupt network waits; local file operations finish before final review. A provider may still charge for an interrupted request.
Every command has an immutable invocation ID and a matching completion record. Guest cancellation checks the recorded supervisor identity, invocation completion, and dedicated UID before confirming termination. It exports before removing the owned guest user and directory. Root-owned lock and cancellation tombstones remain to reject late stage/run requests. A supervisor that disappears without completion proof remains uncertain until reconciliation or a guest reboot.
The controller never stops or deletes the borrowed VM or its network.
An export/cleanup failure keeps guest data and reports **Needs reconciliation**.

An API restart marks active attempts **Interrupted**. Nothing is automatically replayed.
Use **Reconcile attempt** to stop/recover the recorded attempt before retrying or deleting its project.
Reconciliation can reconnect to the same VM after a reboot; the guest boot marker prevents signalling an old supervisor PID in another boot.
If the VM, SSH identity, or retained workspace is unavailable, recovery remains blocked instead of deleting unconfirmed data.
**New attempt** creates a fresh immutable attempt ID and records the previous task as `retryOf`.

Deleting a project removes only its saved conversation and controller-owned attempt copies after execution/reconciliation finishes. Source files and borrowed VMs remain.
Back up the private database and adjacent `.sandbox` directory together if retained code changes are needed.

## CLI and local stdio MCP

All entry points use `/api/ai-sandbox`; the CLI/MCP process does not receive provider credentials.
Use `--api` or an existing `--host` profile to select the same API as the Dashboard.

```sh
firecrab sandbox projects
firecrab sandbox models CONNECTION_UUID --discover
firecrab sandbox verify CONNECTION_UUID MODEL_ID --tools
firecrab sandbox save-project PROJECT_UUID < project.json
firecrab sandbox run PROJECT_UUID TASK_UUID < task.json
firecrab sandbox tasks PROJECT_UUID
firecrab sandbox inspect TASK_UUID
firecrab sandbox cancel TASK_UUID
firecrab sandbox reconcile TASK_UUID
firecrab sandbox apply TASK_UUID src/health.ts
```

`project.json` contains `{name, sourcePath, vmId, selection: {connectionId, model}, instructions}`.
`task.json` contains `{kind: "chat" | "code", input, selection?: {connectionId, model}, retryOf?: UUID}`.
Reuse the same task UUID and identical request after an uncertain admission response. This returns the existing attempt and does not run a second one.
Use `GET /api/ai-sandbox/projects/PROJECT_UUID/tasks?before=TASK_UUID` for an older page and `?since=UNIX_MILLISECONDS` for incremental updates.

Run `firecrab mcp` for the existing stdio adapter.
Its `sandbox_*` tools cover projects, models, verification, task admission, inspection, cancellation, reconciliation, and selected-file apply.
Task start advertises guest execution. Apply advertises destructive source modification and requires explicit reviewed paths.
EOF does not delete retained data or borrowed resources.

Managed environments and retained preview controls are described in
[Sandbox environments](sandbox-environments.md) and
[Sandbox control](sandbox-control.md). Task dependencies and bounded retries
reuse this task lifecycle. Interactive browser terminals belong to Canvas #496,
which remains on hold until AI Sandbox implementation and direct QA are accepted.
