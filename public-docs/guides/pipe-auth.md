# Pipe Auth

Open **Pipe → Pipe Auth** to connect a provider account. Firecrab verifies the
credentials before saving them. Each host keeps one connection per provider.
Supported Pipe services reuse that connection without asking for a second login.

Select a provider row to open its connection details. The credentials editor uses
the dashboard's right detail panel on desktop and a full-width panel on phones.
Use **Connect** or **Update credentials** to open the form. Closing a panel with
unfinished credentials asks before discarding them.

| Provider | Credentials | Available Pipe service |
| --- | --- | --- |
| GitHub | Personal access token (classic) with `read:packages` | GHCR in Pipe Registry |
| AWS | Access key ID, secret access key, region, optional session token | The account's private Amazon ECR registry in Pipe Registry |
| Firecrab | Connection token issued by [console.firecrab.dev](https://console.firecrab.dev/) | Its fixed address in Firecrab Tunnel |

For Firecrab, select **Connect Firecrab** and paste the console's 64-character
connection token. Verification resolves its assigned address without publishing
a target. **Firecrab Tunnel → Add fixed address** selects **Use Pipe Auth** when
a shared token is connected. Select the VM apps or saved Nginx routes to publish.
**Enter a connection token** remains available for another address.

Each host keeps one shared Firecrab token. Updating it changes the token available
for new connections. Existing tunnels keep their own saved tokens. Disconnecting
Pipe Auth does not revoke console tokens or remove tunnels. Revoke or suspend a
token in the console to stop its remote access. Firecrab credentials are never
used for container registries.

GitHub authentication checks the token owner's identity and package scope. If an
organization requires SSO, authorize the token for that organization. GitHub
Packages requires a classic token; see [GitHub registry authentication](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#authenticating-to-the-container-registry).

AWS authentication calls `ecr:GetAuthorizationToken` in the selected region.
The IAM principal also needs `ecr:DescribeRepositories` and permission to pull
the requested repositories.
Firecrab signs requests with AWS Signature Version 4 and supplies the session
token when present. It requests an ECR login token for each registry operation,
so an expired ECR password does not need to be pasted into the dashboard.
Temporary AWS access keys still expire and must be replaced in Pipe Auth.
See [ECR authorization tokens](https://docs.aws.amazon.com/AmazonECR/latest/APIReference/API_GetAuthorizationToken.html).

Pipe Registry displays these registries automatically. Its **Manage in Pipe Auth**
link opens credential rotation and disconnect controls. A failed replacement
keeps the previous connection. Disconnect removes only that provider's shared
authentication. Independently saved registry logins and pulled images remain.
Existing Docker Hub, Harbor, GHCR and ECR direct logins remain supported.
AWS Pipe Auth enables paginated ECR repository discovery. Direct ECR password
logins cannot list repositories. Public ECR still uses direct authentication.

Credentials stay in the existing owner-only SQLite database. They are not
application-encrypted. GET, PUT and DELETE responses never return credential values. Browser storage does not keep the entered credentials. These API routes
use the same request size, origin and concurrency protections as other Pipe
routes:

- `GET /api/pipe/auth`: list connected providers and available services.
- `PUT /api/pipe/auth/github`: verify and save `{ "provider": "github", "token": "…" }`.
- `PUT /api/pipe/auth/aws`: verify and save `{ "provider": "aws", "accessKeyId": "…", "secretAccessKey": "…", "region": "ap-northeast-2", "sessionToken": "…" }`.
- `PUT /api/pipe/auth/firecrab`: verify and save `{ "provider": "firecrab", "token": "…" }`.
- `DELETE /api/pipe/auth/{provider}`: disconnect a provider.

Firecrab metadata returns its address label as `account`, `registry: null`, and
`services: ["firecrab-tunnel"]`. To connect its saved token, send
`{ "usePipeAuth": true, "target": { "type": "nginx", "host": "app.example.com" } }`
to `POST /api/pipe/firecrab-tunnels`. Choose exactly one of `usePipeAuth`, an
explicit `token`, or an existing `subdomain`; secrets stay on the host.

