# Pipe Auth

Open **Pipe → Pipe Auth** to connect a provider account. Firecrab verifies the
credentials before saving them. Each host keeps one connection per provider.
Supported Pipe services reuse that connection without asking for a second login.

| Provider | Credentials | Available Pipe service |
| --- | --- | --- |
| GitHub | Personal access token (classic) with `read:packages` | GHCR in Pipe Registry |
| AWS | Access key ID, secret access key, region, optional session token | The account's private Amazon ECR registry in Pipe Registry |

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
- `DELETE /api/pipe/auth/{provider}`: disconnect a provider.

