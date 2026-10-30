# CODE MicroVM runtime

CODE can provision a dedicated MicroVM and clone its selected GitHub branch into `/home/fc-code/project`.
Start and stop reuse existing VM lifecycle APIs.
Stopping preserves the root disk, Git metadata, local commits, and uncommitted files.
This API does not launch Web VS Code, a terminal session, or an AI agent.

## Provision

Save [workspace configuration](code-workspaces.md) and connect the selected GitHub account first.
Select an installed Ubuntu or Alpine image and an existing internet-enabled MicroNetwork.
Other custom images work only when the required tools are already present.
The guest requires `/usr/bin/git`, Python 3 and `timeout`.
CODE never creates or deletes the selected network or storage root.

Send `POST /api/code/workspaces/{id}/runtime`:

```json
{
  "expectedRevision": 1,
  "template": "ubuntu-26.04",
  "microNetworkId": "33333333-3333-4333-8333-333333333333",
  "storageRoot": null
}
```

The operator identity and REST origin checks are the same as the configuration APIs.
Unknown request fields are rejected, including credentials, guest paths, commands and VM IDs.
The server pins the installed image's version and content hashes.
RAM, CPU and disk come from the saved configuration, with existing VM image and storage validation.
Changing an image alias after admission does not select another image for a retry.

The response is `202` with a durable operation ID and server-reserved VM ID.
Poll `GET /api/code/workspaces/{id}/runtime` until `phase` becomes `ready` or `failed`.
These responses have `Cache-Control: no-store`.

| Phase | Meaning |
| --- | --- |
| `queued` | Durable operation waiting for a worker |
| `provisioning` | Reserved VM creation or boot in progress |
| `preparing` | SSH readiness, tools and repository initialization in progress |
| `ready` | Working folder confirmed for the recorded live VM runtime instance |
| `stopping` | Guest filesystem sync and VM stop in progress |
| `stopped` | Guest stopped and disks retained |
| `failed` | Operation failed; retained resources can be inspected and retried |
| `reconcile_required` | Live VM observations no longer match recorded readiness |

`runtimeId`, `initialCommit`, `projectPath`, `placement`, and fresh `vmState` observations describe the retained runtime.
`ready` refers to the working folder and matching running VM, not to agent authentication.
Missing or restarted VMs cannot inherit a previously returned `ready` observation.

## Guest setup and checkout

The controller authenticates the selected GitHub login and reads the exact branch reference before initial VM creation.
It repeats the check before transferring clone credentials.
Trusted toolchain setup happens first, so clone credentials are supplied only when the checkout needs them.
The existing per-VM operator SSH key and pinned guest host key authenticate guest preparation.
SSH readiness has a 120-second retry deadline.
Preparation has a guest-side timeout and a bounded host transport timeout.

When tools are missing, only trusted Ubuntu/Debian `apt-get` or Alpine `apk` package installation runs as root inside the guest.
The controller host's packages and users are not modified.
Runtime families provide Node.js/npm, Python, or Rust/cargo from the selected image or distribution packages.
Runtime versions follow that image and package repository.

The bootstrap creates a dedicated `fc-code` account with a workspace-specific ownership tag.
Repository Git operations run with that account's UID and GID, not root.
Existing unrelated accounts, homes, project paths, and symlink substitutions are rejected.
The repository URL is fixed to `https://github.com/{owner}/{repository}.git`.
The exact selected branch is fetched, its commit is compared with GitHub's observed reference, and a working branch is checked out.
A branch that advances during initialization produces a retained failure; retry to inspect its new reference.

The token travels through pinned SSH input and a transient Git fetch process environment.
It is not inserted into the VM's saved environment, Git URL, repository configuration, operation journal, or process arguments.
The temporary askpass program contains no credential value and is removed after use.
Git documents this prompt mechanism in [Providing credentials](https://git-scm.com/docs/gitcredentials).
Global/system Git configuration, credential stores, hooks, redirects and automatic submodules are disabled for these operations.
These controls use Git's [configuration options](https://git-scm.com/docs/git-config).
No project package installer, repository hook or project script runs during checkout.

Checkout happens in a private staging directory.
A root-owned journal records publication before the directory becomes the project folder.
Recovery completes a prepared publication or retries an unpublished clone without replacing an existing project.
Resume validates the workspace marker, original repository URL and Git metadata.
It preserves local commits and dirty/untracked files, and does not fetch or reset the branch.
A confirmed checkout can resume without a connected GitHub account.
A completed guest-side publication is also recoverable when the API restarted before recording its commit.

## Start, stop and retry

Use the same positive, current configuration revision for lifecycle requests:

```json
{ "expectedRevision": 1 }
```

| Method | Path | Effect |
| --- | --- | --- |
| POST | `/api/code/workspaces/{id}/runtime/start` | Boot and resume the existing VM; retry retained failures |
| POST | `/api/code/workspaces/{id}/runtime/stop` | Confirm guest filesystem sync, then stop the VM |
| POST | `/api/code/workspaces/{id}/runtime/detach` | Release the binding to a stopped or absent VM; retain the VM and disk |

Start and stop return `202` with a pollable operation.
Duplicate requests for an active action reuse its operation and VM IDs.
A conflicting active action returns `409 code_runtime_busy`.
Stale configuration revisions return `409 code_workspace_revision_conflict`.
Different placement for an existing binding returns `409 code_placement_bound`.

Filesystem sync must succeed before a running guest is stopped.
If sync cannot be confirmed, the operation fails and the guest is retained.
Code operations never delete working files, disks, networks or storage roots.
Failures report stable `error` categories without guest output, provider bodies or credentials.

Only two CODE workers run concurrently.
Per-workspace process locks exclude duplicate workers across API processes.
The database reserves ownership before creating a VM, and fences journal writes by operation ID.
After normal VM/network startup recovery, maintenance resumes unfinished journal operations.
Partial VM creation uses the existing idempotent SSH-key and lease recovery function.
A VM recorded as created is not silently replaced when it disappears.
Explicit retry keeps the same reserved VM UUID.

## Change or remove configuration

While a runtime is bound, repository owner/name/branch, runtime family and resources cannot change.
An idle runtime permits workspace renaming and compatible AUTH reference selection.
All configuration writes are blocked during an active lifecycle operation.
Deleting bound configuration returns `409 code_runtime_bound`.

Stop and detach before changing the physical configuration or deleting its metadata.
Detach retains the VM in the ordinary VM inventory and returns its ID.
It does not erase the project folder or disconnect shared accounts.
The retained VM can then be managed or deleted explicitly with ordinary VM controls.
While still bound, ordinary VM mutation routes reject start, stop, pause/resume, edits, storage moves, and deletion with `409 code_vm_owned`.
Use CODE lifecycle controls so another route cannot bypass its ownership and filesystem-sync checks.
