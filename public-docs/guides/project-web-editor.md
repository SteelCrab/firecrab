# Project Canvas and Web VS Code

The AI Canvas opens the retained folder of an existing Sandbox project. It uses the project file APIs, private view APIs, and native terminal APIs. Project setup and volume verification must complete before live file operations or editor execution.

## Independent tabs

The plus button creates an empty canvas. Choose a project explicitly. A new tab does not copy another tab's panels, private notes, or editor documents.

Canvas layouts are saved to the project SQLite database under the verified caller and a stable view UUID. Browser storage contains tab identities. Select **Saved canvases → Reopen** to open a previously saved private canvas. Closing a tab retains its server-side view and sessions. The editor client can remain hidden so pending private drafts can finish.

## Personal Page

Select **Personal Page · Web VS Code** or open a file from the shared folder. Each caller/view pair has its own editor identity, native UID, token, and private editor data. Saved project files are shared. Unsaved buffers and editor state belong to that caller/view pair.

The pinned OpenVSCode server runs inside the project's verified workload VM as an unprivileged native terminal process. The controller never passes Pipe AI provider credentials into the guest. The editor proxy checks project and VM execution rights, caller ownership, and runtime binding. Editor hosts cannot call controller, provider, or other project APIs.

The editor uses a virtual `firecrab:` filesystem. Reads and saves go through the authorized project file API. A save includes the file version it was based on. If another tab changed the saved file, the old buffer remains dirty and the save fails. Preserve the draft and compare the latest saved file before retrying.

Private buffers retain their saved base version. An accepted private draft survives API restart and VM restart. Discarded buffers retain a revision tombstone that rejects stale requests. The extension provides **Firecrab: Open Unsaved Draft Recovery Copy** and **Firecrab: Retry Private Draft Persistence** commands.

## Terminals

Add a terminal to create a new native shell. Use the retained-session selector to attach an existing terminal. Closing a panel disconnects its client and releases its input lease. It does not replay a command or create a replacement shell. A terminal interrupted by VM shutdown keeps its recorded output.

## Current support

- Scoped `*.editor.localhost` origins on the controller's port.
- Pinned OpenVSCode 1.109.5 on Linux arm64/x64 with glibc. The real editor gate uses Ubuntu 26.04; Alpine is not an editor target.
- Authorized file reads/saves, private buffers, independent views, shared directory navigation, native terminals, and retained file-history listing.
- Folder creation, file rename, and deletion currently use a project terminal. The virtual filesystem rejects those IDE operations.
- IDE extensions must support virtual workspaces. Native workspace extensions can have additional limitations.

This is an implementation slice of #550. History compare/restore controls, final editor-session cleanup, remaining model/skills integration, production service-user QA, and cross-host restore acceptance remain pending. Do not treat this guide or its screenshots as full issue acceptance.

See [private views](project-private-views.md), [workspace files](project-workspace-files.md), and [real editor recovery evidence](../development/project-editor-recovery-2026-11-01.md).
