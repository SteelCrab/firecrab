# Official project skills

Canvas folder agents and the Personal Page share a project skill catalog. It is
stored in the project's private SQLite database, independently of browser sessions.
Schema 9 adds catalog and installation tables without replacing existing data.

## Sources and runtime adapter

The supported repositories are [OpenAI Plugins](https://github.com/openai/plugins)
and [Anthropic Skills](https://github.com/anthropics/skills). Explicitly refresh a
source to discover an exact commit and its marketplace entries. Search and source
filters operate on the retained catalog. Reading that catalog requires no network.

OpenAI plugin skill bundles and their individual skill directories have different
labels. A bundle installs its skill files. It does not activate plugin apps, MCP
servers, connectors, hooks, agent definitions or dependency installers. Marketplace
entries that redirect to a different publisher/repository are not followed. The
adapter never accepts arbitrary repository URLs or uses a GitHub App/token.

The runtime adapter is Firecrab's existing Sandbox folder agent. OpenAI/ChatGPT
requests can select OpenAI-source skills; Claude requests can select Anthropic-source
skills. A different model provider leaves the prior selection inactive. It does
not silently convert that selection or use another account. This is instruction
and resource integration, not the native Codex/Claude plugin installer.

## Install and select

1. Open **Official skills** in a folder agent, in Canvas or beside Web VS Code.
2. Refresh a source, then search or filter its catalog.
3. Install a skill or plugin skill bundle in the current project.
4. Select its confirmed revision for the agent session.
5. Submit an authorized task with a verified compatible model.

The source commit and installation root are displayed. Actual files are written
under `/workspace/shared/.agents/firecrab-skills/<installation-id>/`. Source paths
and bytes are preserved. Ancestor license/notice files are retained under
`.source-licenses/` with their repository paths. Existing versioned file APIs create directories and keep
one exact operation UUID per file. Scripts are installed as bytes; executable bits,
dependencies and background hooks are not activated. Use an explicit interpreter
through the bounded shell tool when a script is appropriate and dependencies exist.

Each browser saves the installation request ID/body before sending it. A lost
response retains **Retry saved installation**. An existing pending installation
has **Resume installation** with the same server-owned file receipts. Neither
retry creates a new install identity or downloads a changed commit. A definitive
rejection with a confirmed absent server record releases the saved draft so another
package can be selected. Network failures retain the original identity.

Per-session selections stay in private Canvas views and appear in both Canvas and
the Personal Page. Installation records and files are project-shared. A pending
agent request cannot change its selected IDs during retry or reconnect.

## HTTP contract

Paths below are relative to `/api/ai-sandbox/projects/{project}/workspace/skills`.

| Operation | Endpoint | Behavior |
| --- | --- | --- |
| GET | `/catalog` | Read the last accepted source catalogs, including while the VM is stopped. |
| POST | `/catalog/{source}` | Explicitly refresh `openai` or `anthropic`. Failure retains prior results. |
| GET | `/installations` | List pending, installed and removed revisions. |
| GET | `/installations/{id}` | Resolve an ambiguous installation response by the original UUID. |
| PUT | `/installations/{id}` | Admit or resume the same pinned package. |
| DELETE | `/installations/{id}` | Disable membership for future requests and retain historical data. |

Example installation body, using the revision and path returned by the catalog:

```json
{
  "source": "anthropic",
  "kind": "skill",
  "path": "skills/brand-guidelines",
  "revision": "<40-character-commit-from-the-catalog>"
}
```

A different body for an existing UUID conflicts. Removed IDs remain removed;
reinstallation requires a new UUID. New installation requires initialized retained
storage and project/VM execution rights. Source discovery and catalog mutations
require project Read/Write. Project readers may inspect accepted catalog metadata.

Folder task requests add `selectedSkillIds`, an ordered array of installation UUIDs.
The server rejects duplicates, incomplete/removed revisions and incompatible sources.
It verifies saved files against their installed hashes before task admission. A
modified or missing installed file requires reinstallation. Admitted instruction
bytes and resource versions are retained by the existing canonical Sandbox task.

The read-only `read_skill_file` tool accesses only files in those admitted revisions.
It returns saved history bytes, not later edits or arbitrary filesystem paths.
The model receives exact selected SKILL.md guidance and revision provenance. Source
`allowed-tools` declarations do not add permissions. Scripts require the explicit
`projectShell` capability; the controller never runs package code.

## Interruption, removal and bounds

A complete, pinned package is downloaded before the installation is admitted.
The private project DB records its bytes and every file UUID before guest effects.
After each confirmed write, progress is saved. Only a complete set of receipts
changes the record to `installed`. Interrupted packages stay `pending` and cannot
be selected. Resume uses frozen bytes and the original versioned write receipts.
After VM restart, verify/reconcile the retained mount before resuming writes.

Remove disables future admissions atomically, including while the VM is stopped.
It does not cancel already accepted tasks. Accepted tasks retain their exact rules
and immutable resource-history versions. Installed/partial files, package bytes and
write history stay retained for recovery and matched backups. Removal is not a disk
purge. Retention/deletion maintenance remains an integrated #550 acceptance gate.

Downloads use fixed HTTPS hosts, reject redirects and partial Git trees, validate
regular Git blob identities and reject selected symlinks/submodules. Packages are
limited to 128 files and 2 MiB total. SKILL.md requires valid YAML metadata, UTF-8
without NUL, and at most 32 KiB. A project retains at most 64 install records and
64 MiB of installation documents. Existing file-history quotas also apply; data is
not silently evicted when limits are reached.

A request selects at most eight revisions, 128 source files in total and 64 KiB of
instruction text. Individual text resource reads are limited to 8 KiB. Source files
remain shared project files; the adapter does not provide a hermetic filesystem or
install missing runtime dependencies.

## Validation boundary

Unit and browser fixtures are distinct from actual provider/workload acceptance.
Manual source validation downloads real OpenAI and Anthropic packages and checks
pinned Git blob hashes without executing package code. Full #550 acceptance still
requires installation/use/restart/removal on an authorized workload VM with real
ChatGPT and Claude connections. The current test Pipe AI connection is unavailable.
PR #562 remains Draft and unmerged against `release/v0.5.0`.
