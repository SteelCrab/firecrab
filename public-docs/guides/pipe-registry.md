# Pipe Registry

Pipe Registry connects to Docker Hub, GitHub Container Registry (GHCR), Amazon
ECR, and Harbor. It pulls OCI container images to the API host without a Docker
daemon. Image uploads are managed by the source registry.

1. Select **Add registry** and choose the provider.
   - Docker Hub: enter your username and a personal access token with repository
     read access. This login is shared with the existing Docker Hub settings.
   - GHCR: enter your GitHub username and a personal access token (classic) with
     `read:packages`. Authorize organization SSO if required.
   - Harbor: enter the registry hostname and optional port, a Harbor username or
     robot account, and its password or token. The account must have repository
     listing and pull access. Use the hostname only, without a URL scheme or path.
     Remote Harbor connections require HTTPS. Loopback development registries,
     such as `127.0.0.1:18080`, use HTTP.
   - ECR: enter the registry hostname and the output of
     `aws ecr get-login-password --region REGION`. Pipe uses username `AWS`.
     Tokens expire after 12 hours. Select **Update token** to renew them.
2. Saving a login automatically loads its repository list. **View images** opens
   the list for another saved registry. Docker Hub and GHCR use the saved account
   namespace. Harbor lists all repositories that the saved account can access
   across its projects. Docker Hub lists only the saved user’s repositories.
   A Docker Hub login is required. Official images are not used as a fallback.
3. Pipe selects the first repository and loads its tags automatically. Select
   another repository to load its tags. **More images** and **More tags** load
   further pages. **Refresh repositories** refreshes the repository list.
4. Select a tag and then **Pull image**. Pulling stays disabled while the tag
   lookup is running or has failed. Failed lookups can be retried. Repository,
   tag, and digest text fields are not shown. Changing the source registry clears
   the previous selection.

ECR login passwords cannot call the signed AWS `DescribeRepositories` API.
Automatic ECR repository discovery requires an AWS IAM integration, which is
not currently available. Pipe shows this limitation without a manual repository
input.

Pulls run as background jobs. Pipe polls the status and retains the transfer ID
across browser reloads. Jobs stop if the API process stops; downloaded images
remain on disk. Two pulls may run at once. A retry reuses verified local blobs.
The Registry screen does not show a local OCI image inventory or conversion panel.

A pull retains the selected platform's config and compressed layers. It does not
retain other platforms, signatures, or attestations. These retained images are
separate from M2Image packages and the MicroRegistry catalog.

Credentials are write-only in API responses. They are stored in the owner-only
SQLite database. Removing a login retains downloaded images. Credentials are
scoped to their exact registry, including its port. A custom Harbor hostname must
be registered before Pipe accepts its tag queries or pull requests. Pulls verify
SHA-256 content digests.

| Endpoint | Action |
| --- | --- |
| `GET /api/pipe/registries` | Saved registry hosts, providers, and usernames; no secrets |
| `PUT /api/pipe/registries/{registry}` | Save `{username, secret}`; include `provider: "harbor"` for a custom Harbor host |
| `DELETE /api/pipe/registries/{registry}` | Remove one saved login |
| `GET /api/pipe/registries/{registry}/repositories?page=…` | Repository names, `nextPage`, and `listingSupported`; Docker Hub uses the saved username |
| `GET /api/pipe/registry-tags?reference=…&last=…` | One tag page and optional `nextLast` cursor |
| `POST /api/pipe/registry-transfers` | Start `{action: "pull", reference}`; returns 202 |
| `GET /api/pipe/registry-transfers/{id}` | Transfer status; ID is the POST response's `alias` |
| `GET /api/pipe/registry-images` | Retained OCI image metadata for API clients |

Harbor discovery uses its `/api/v2.0/repositories` API. Tag lookups and pulls use
its OCI Distribution API and registry token service. Catalog pagination does not
follow provider-supplied URLs with saved credentials.

Run the isolated dashboard tests with `npm run test:pipe --prefix firecrab-e2e`.
They use their own Vite server on port 8096 and API fixtures. Backend tests use
loopback registries and catalog services. Live integration checks require a
running registry and credentials.
