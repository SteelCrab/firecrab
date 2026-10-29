# AI Canvas

Open **AI → AI Canvas**, or `/#/ai-canvas`.
Choose a Sandbox project to view its AI conversation and task results together.
Send conversations and code tasks through the existing [AI Sandbox](ai-sandbox.md) lifecycle.
Review code changes before applying them.

## Shared conversation

The **Shared chat** panel stores messages separately from AI task history.
The model does not receive these messages automatically.
Authenticated participants with project read access can see messages.
Sending also requires project write access.
Project permissions do not grant model or MicroVM execution access.
See [Sandbox access](sandbox-access.md) to configure authenticated host grants.

Messages are plain text and are limited to 4,096 UTF-8 bytes each.
The server rejects messages containing a currently connected account credential.
A project can retain up to 10,000 messages.
Messages survive browser disconnect and controller restart.
Deleting the Sandbox project deletes its shared messages.
The browser polls for new messages and displays up to the most recent 200 messages it has loaded.
If a send response is uncertain, the browser retries with the same message ID.

This first workspace does not yet provide shared terminal sessions, invitation links, or Canvas-specific roles.
It does not automatically provision a MicroVM.
