# React dashboard migration

This dashboard is the React replacement for the Rust/WASM dashboard. The
terminal uses xterm.js. Its production build has no WebAssembly assets.

The `service dev` launcher uses this directory. Release packaging builds this React dashboard. All dashboard routes run in React; the Rust/WASM frontend and its build pipeline are removed.

The React screens include CODE workspaces, MicroVMs, pools, schedules, Stack, cluster,
MicroService, security groups, Pipe Auth, AI, S3 and databases, Queue, Secrets,
MicroBlockStorage, images, kernels, Shell, host and AI Sandbox. Automated
browser tests cover resource editing, database maintenance, service releases,
Sandbox resources and Pipe workflows using local fixtures. The live private
instance is checked read-only to keep its existing VMs and configuration intact.

To keep an existing development instance and its VMs, run from this checkout:

```sh
cargo run -p firecrab-cli -- service dev --dashboard-only --connect-source /path/to/existing/worktree --port 8080
```

The selected instance must already be running. This foreground command serves
only the dashboard and proxies its requests to that instance's existing API.
It does not deploy guest binaries, reboot the management VM or move VM data.
Ctrl+C stops only this dashboard. The frontend reloads when its source changes.

For standalone Vite development, explicitly select the intended API port:

```sh
npm ci
FIRECRAB_DEV_API_PORT=31100 npm run dev
```

Use the API port printed by the selected `service dev` instance instead of the
example. The server defaults to port 8080. Its resource controls stay disabled
unless `FIRECRAB_DEV_RESOURCE_PORT` is also provided. Both proxies target only
localhost. This prevents an isolated preview from silently using the primary
controller on port 5524.

Build and test:

```sh
npm ci
npm run build
npm test
```

Browser tests require Playwright's Chromium and run only against local fixture
servers. They do not start real services or change the running development instance.
