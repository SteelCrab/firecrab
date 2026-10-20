# Pipe Registry

The **Pipe Registry** card connects Pipe to Docker Hub, GitHub Container Registry
(GHCR), Amazon ECR private registries, and `public.ecr.aws`. It pulls OCI
container images to this host. No Docker daemon or AWS CLI is required on the
API host. Pipe Registry does not push images.

1. Select **Add registry**. For Docker Hub, enter the Docker Hub username and a
   personal access token with repository read access. Pipe shares this login with
   the existing Docker Hub settings. For GHCR, enter the GitHub username and a
   personal access token (classic) with `read:packages`. Authorize organization SSO when
   required.
2. For ECR, enter the registry hostname, such as
   `123456789012.dkr.ecr.ap-northeast-2.amazonaws.com`. Paste the output of
   `aws ecr get-login-password --region ap-northeast-2`. Pipe uses username `AWS`.
   Tokens expire after 12 hours. Use **Update token** to renew them. Public ECR
   uses `aws ecr-public get-login-password --region us-east-1`.
3. Select a source registry and enter its repository, such as `team/app`.
   For Docker Hub official images, enter `nginx` or `library/nginx`. The API also
   accepts shorthand references such as `nginx:alpine` and the explicit
   `docker.io/library/nginx:alpine` reference.
   **List tags** lists tags in pages of 100; **More tags** loads the next page.
   Select a tag or enter a `sha256:…` manifest digest. **Pull image** downloads
   and verifies the selected platform's config and layers on this host.
   Public images can be pulled without a saved login.
4. Under **Local OCI images**, select **Import as M2Image** to convert a pulled original image into a bootable
   VM template. This uses its pinned digest and the existing OCI import pipeline.
   Its platform and guest compatibility requirements still apply.

Pulls run as background jobs. The card polls their status and retains its
current job ID across browser reloads. Jobs stop if the API process stops;
completed images remain on disk. Refresh the local image list after an
interrupted job and retry as needed. Two pulls may run at once. A retry reuses
verified local blobs.

A pull keeps the selected platform's config and compressed layers. It does not
keep other platforms of a multi-platform index, signatures, or attestations.
Only images pulled through this card appear in this local inventory. This inventory is separate from
M2Image packages and the MicroRegistry catalog. Converting an OCI image does not
change the retained original.

Login tokens are write-only in API responses. They are stored in the same
owner-only SQLite database as the existing Docker Hub login. Removing a login
retains local images. Credentials are scoped to their exact registry; HTTPS is
required for external registries. Pulls verify SHA-256 content digests.

| Endpoint | Action |
| --- | --- |
| `GET /api/pipe/registries` | Saved registry hostnames, providers, and usernames; no tokens |
| `PUT /api/pipe/registries/{registry}` | Save or rotate `{username, secret}` for Docker Hub, GHCR or ECR |
| `DELETE /api/pipe/registries/{registry}` | Remove one saved login |
| `GET /api/pipe/registry-tags?reference=…&last=…` | One tag page and optional `nextLast` cursor |
| `GET /api/pipe/registry-images` | Original OCI images retained on this host |
| `POST /api/pipe/registry-transfers` | Start `{action: "pull", reference}`; returns 202 |
| `GET /api/pipe/registry-transfers/{id}` | Transfer snapshot; ID is the POST response's `alias` |

The existing `/api/oci/inspect` and `/api/oci/import` endpoints also use these
saved registry logins. MicroService deployment uses the matching registry login
as well. Docker Hub logins saved or removed through Pipe use the same credential
as `/api/microregistry/docker-hub`. The list includes an existing Docker Hub login
as `docker.io` without exposing its token. The API also accepts the registry
alias `registry-1.docker.io` without creating a second login.
See the [Docker Hub token documentation](https://docs.docker.com/security/access-tokens/),
[GHCR authentication documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)
and [ECR authentication documentation](https://docs.aws.amazon.com/AmazonECR/latest/userguide/registry_auth.html)
for account permissions and token issuance.

Run the isolated dashboard tests with `npm run test:pipe --prefix firecrab-e2e`.
They start their own Vite server on port 8096 and use API fixtures. Backend
registry tests use loopback HTTP registries with Basic and Bearer authentication;
they cover tag paging, pulls, token-service checks, saved credentials, and API
origin protection. Live Docker Hub/GHCR/ECR account pulls require operator-supplied
credentials and are not part of these fixture tests.
