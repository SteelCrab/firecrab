# AI Sandbox

AI Sandbox opens the native multi-agent workspace at `/#/ai-sandbox`.
Select a MicroVM, connect a coding CLI, and submit a task. No separate Sandbox project setup or model-task workspace is required.

## Manage CLI agents in a MicroVM

The workspace runs coding CLIs inside the selected MicroVM. Project folders and login credentials stay in that VM. It does not accept a host project path or copy a host CLI account into the VM.

1. Select a regular MicroVM or use **New MicroVM** to create one without leaving the workspace. Start or resume it before connecting. Internal service VMs cannot be selected.
2. Select **GitHub AUTH**. **Prepare GitHub AUTH** installs only the GitHub tools when needed; selecting or installing a coding CLI is not required. Copy the one-time code, open **Authorize on GitHub**, and approve **GitHub CLI** in the browser. The screen detects completion automatically. Pending requests survive browser/native-service restarts. An expired or denied request can be started again without an HTTP 409.
3. Choose a repository from the authenticated account, including private repositories, or enter `owner/repository`. Connect it once to this MicroVM. All new tasks and Web VS Code use `/workspace/firecrab/project`; new tasks do not clone again, reset local edits or switch branches. Use a different VM for another repository. An empty VM folder can also be initialized explicitly. Existing sessions keep their original folders.
4. Choose **Codex** or **Claude Code** explicitly. **Prepare CLI environment** installs Herdr and the selected coding CLI inside that VM. Automatic installation supports Debian, Ubuntu and Alpine. Select **Connect CLI account** and finish its login terminal. Then enter the task and select **Start task**. GitHub AUTH and coding-provider authentication are separate. No model task runs during authentication.
5. If the CLI asks for first-run setup or project trust, finish that prompt in its terminal. **Send waiting task** then delivers the saved task explicitly. An uncertain delivery is shown for inspection instead of being replayed automatically.
6. Follow tasks in the default **Board** view. Cards show their coding CLI and move between **Waiting**, **Working**, **Needs attention**, and **Done** as the native runtime reports progress. Select a card to open its terminal. In **Terminals**, select up to four sessions in the side list. Click a terminal to type directly into that CLI. The **…** menu contains interruption, automation and session closure. Closing a session requires confirmation and retains its final output. No automatic commit, push or pull request is made.

**New task** lets you choose a connected Codex or Claude Code account for that task. Several tasks can run in parallel in the same VM workspace. A CLI session is created only when **Start task** is submitted. Opening the board, switching views and reloading do not launch or resend work. The view choice is saved per VM. Account login sessions stay outside the task board; CLI setup and uncertain task delivery remain visible for inspection.

The native localhost dashboard and `service dev` use the existing management connection to reach the selected VM. Each managed home and VM gets separate Firecrab session metadata and a private named [Herdr](https://herdr.dev/tui/docs/agents/) runtime. The bridge validates the controller, VM UUID, running state and pinned guest SSH key before connecting. It never falls back to a host coding CLI. A recycled terminal generation cannot receive input through an older session reference.

Browser reloads do not stop CLI sessions. Stopping the selected VM disconnects them; restart it and inspect the session before retrying work. Preparing a CLI or closing a session does not start, stop or delete the VM. Files remain on the VM disk.

Installed GitHub Copilot commands can also be used. Automatic installation and sign-in controls cover Codex, Claude Code and GitHub CLI. Model overrides are supported for Codex and Claude Code. Provider usage can incur charges when the user starts work.

Installation and authentication follow the official [Codex CLI](https://learn.chatgpt.com/docs/codex/cli), [headless Codex authentication](https://learn.chatgpt.com/docs/auth#login-on-headless-devices), [Claude Code setup](https://code.claude.com/docs/en/setup) and [Herdr installation](https://herdr.dev/tui/docs/install/) instructions.

## Verification, GitHub Actions and tunnel previews

Open a coding session's **… → Automation and preview** menu.

1. Review **Execution settings**. Commands run in that task's existing MicroVM workspace. Install the project's runtime/toolchain in the VM first. Test commands must finish; `CI=true` disables watch mode in tools that support it. The optional test app must stay in the foreground and listen on `127.0.0.1` or `0.0.0.0` at the selected port.
2. **Run verification** runs tests, an optional Trivy scan, the optional build and the optional test app in that order. A failure stops the sequence. High/critical dependency vulnerabilities, exposed secrets and configuration findings block the build/app stages. Missing scanners, database download failures and incomplete reports are not reported as a clean scan. The first scan installs the checksum-verified pinned Trivy release.
3. The job runs inside the VM with a workspace lock. Browser/native-server restarts do not restart it. A VM reboot interrupts it. Results and bounded displayed logs remain available. Stop the owned test app before another run; other processes are never stopped by PID alone.
4. **Create workflow** creates `.github/workflows/firecrab-checks.yml` in the VM. It runs the configured tests, security scan and build on pushes, pull requests and manual dispatch. It uses a pinned checkout action and read-only repository permissions. Existing files are not overwritten. Inspect and push the changes separately; creation does not publish a commit, PR or production deployment.
5. **Load workflow runs** reads recent GitHub Actions runs through the GitHub account connected inside the VM. Each run shows its branch, commit and result. This is separate from the local verification result.
6. After verification passes and the app is ready, **Create test URL** starts a dedicated Cloudflare Quick Tunnel inside the selected VM. The first use downloads the official connector and verifies its GitHub-published SHA-256 digest. The temporary HTTPS address is public. Browser/native-server restarts find the same recorded process. Stopping or deleting the VM ends the connection; an IP address reused by another VM cannot inherit it. Existing Pipe routes and tunnels are untouched.
7. **Check HTTPS** makes an explicit external request to this task's recorded Quick Tunnel origin. It displays the HTTP status and response time without following redirects. A connector's running state alone is not proof that public HTTPS works. **Stop tunnel** stops that connector. **Stop test app** stops its tunnel first, then only the recorded app process group after checking its boot and process start identity. A stopped tunnel can be started again while the verified app remains running.

This provides test-environment deployment. Production release targets, credentials and approval rules must be configured separately. No production deployment is performed automatically.

## Control an agent terminal

Click the task's terminal to type directly into its existing CLI session. Enter, Tab, arrow keys, Ctrl+C and paste go to that terminal. There is no separate reply box or send button. Resize the pane to resize its PTY. Scroll inside the terminal to read its history; hold Shift to select text when the CLI uses the mouse.

Closing a browser tab or deselecting a task releases terminal control without stopping the agent. **Reconnect terminal** attaches to the same session. Input is never replayed after a disconnect. A session controlled by another client is not taken over automatically. **End session** explicitly stops its CLI. Completed sessions retain read-only output. Verification blocks new terminal input while its workspace is being checked.

## Edit task files with Web VS Code

1. Select a coding task and choose **Web VS Code** beside its workspace path.
2. On first use, Firecrab installs the official OpenVSCode Server inside that MicroVM. The connection page shows installation progress.
3. The editor opens `/workspace/firecrab/project`, the same VM folder used by every new CLI task. Existing sessions retain `/workspace/firecrab/TASK_UUID`. Saving edits changes those files directly. Guest-side changes appear in the editor too.
4. Reopening the editor reuses its recorded server and private SSH connection. It does not create another project or copy files to the desktop.

The editor uses a token-protected guest loopback listener and a pinned SSH loopback forward. It does not publish a test tunnel. Workspace Trust prompts remain enabled. Login sessions do not expose an editor button.
