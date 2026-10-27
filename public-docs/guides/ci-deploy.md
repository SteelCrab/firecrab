# Service CI Deploy

CI replaces the image of one running MicroService. The existing Deploy queue
performs readiness checks, Nginx traffic switching, drain, and failure recovery.
The CI API cannot edit configuration, reveal Secrets, start a stopped service,
or manage VMs. A deployed image still receives this service's existing Secret bindings.

## Prepare the service

1. Deploy the service through the operator API or Dashboard. Configure its domain
   and a matching registry account in [Auth](pipe-auth.md).
2. Update the host bundle and apply the current Pipe configuration. The service's
   existing [tunnel](pipe.md) must already provide verified public HTTPS.
3. Open **CI tokens** in the service's Deploy panel. Enter a name and lifetime,
   then use the **Token** icon. Copy the value into the CI secret store.

The token appears once. Closing, refreshing, or leaving the panel removes it.
Its hash persists on the host. Metadata includes the exact service UUID,
repository, creation time and expiry. The default lifetime is 30 days; the API
accepts 60 seconds to 90 days. Each service permits twenty credentials.
Use **Revocation** to revoke one immediately, including an expired credential.

Operator CLI equivalents:

```sh
firecrab microservice tokens SERVICE_UUID create --name github --expires-in 2592000
firecrab microservice tokens SERVICE_UUID list
firecrab microservice tokens SERVICE_UUID revoke TOKEN_UUID
```

Creation prints the token once in JSON. Do not commit that output or put the
token in command arguments. Store it as `FIRECRAB_DEPLOY_TOKEN` in the runner's
secret environment. Tokens never provide registry login: image pulls still use
the service host's connected Auth account.

## CLI

```sh
firecrab microservice ci-deploy \
  --url https://orders.example.com/__firecrab/deploy \
  --image ghcr.io/team/orders@sha256:YOUR_64_CHARACTER_DIGEST
```

The CLI prints an operation UUID before submission and waits for that exact
operation. `--operation-id UUID` resumes it after a lost response or client exit.
`--expected-release UUID` requires the reviewed active release; otherwise the
client reads it before submission. Concurrent changes return `409`.
`--strategy blue_green` selects blue-green instead of rolling.
`--timeout SECONDS` bounds waiting; timeout does not cancel accepted work.
`--token-env NAME` selects another secret environment variable.

The image must use the token's exact repository and an immutable SHA-256 digest.
An OCI index digest may resolve to a different verified host-platform digest.
The successful result reports that platform digest.

## GitHub Action

After building and pushing your image, pass its immutable reference to the
[composite Action](https://docs.github.com/en/actions/tutorials/create-actions/create-a-composite-action).
The Action requires Python 3.10 or later and no Python packages or Firecrab installation.

```yaml
jobs:
  deploy:
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: SteelCrab/firecrab/.github/actions/deploy@main
        id: deploy
        with:
          service-url: https://orders.example.com/__firecrab/deploy
          token: ${{ secrets.FIRECRAB_DEPLOY_TOKEN }}
          image: ${{ needs.build.outputs.image }}
          timeout: '1800'
```

Pin the Action ref to a reviewed commit for production. The default operation UUID
is stable for a run/job/step and service URL, including workflow reruns. Supply
`operation-id` when coordinating another retry identity. Optional inputs are
`expected-release`, `strategy`, and `timeout`. Outputs are `operation-id` and `digest`.

The Action and CLI fail on a failed/cancelled attempt, invalid credentials, changed
inputs, or timeout. They do not poll only the active release: recovery can restore
an older healthy release while the requested attempt remains failed.
Terminal CI outcomes persist independently of later Stop, rollback, or release
history pruning. A failed attempt needs operator recovery and a new operation
UUID for a new CI deployment. Revocation/expiry stops new requests and polling;
already accepted work continues under its saved Deploy journal.
An operation remains bound to the credential that admitted it.

## API and boundary

| Interface | Route |
| --- | --- |
| Operator token metadata / issuance | `GET, POST /api/micro-services/{id}/deploy-tokens` |
| Operator revocation | `DELETE /api/micro-services/{id}/deploy-tokens/{tokenId}` |
| CI current release / submission | `GET, POST https://SERVICE/__firecrab/deploy` |
| CI exact attempt | `GET https://SERVICE/__firecrab/deploy/{operationId}` |

CI sends `Authorization: Bearer TOKEN`. POST accepts only:

```json
{"operationId":"UUID","expectedReleaseId":"UUID","image":"ghcr.io/team/orders@sha256:DIGEST","strategy":"rolling"}
```

Nginx maps the reserved path to that service's UUID on the dedicated
`127.0.0.1:5525` listener. This listener has no operator or Dashboard routes.
The installed systemd unit enables it with `FIRECRAB_CI_DEPLOY=1`; source runs
can explicitly set the same variable. Keep this listener on loopback.
A local runner may use `http://127.0.0.1:5525/api/ci/services/UUID/deployments`.
Remote plaintext HTTP, URL credentials, and redirects are rejected by both clients.
TLS verification remains enabled and uses trusted roots, including `SSL_CERT_FILE`.

Responses exclude configuration, Secret references, credentials, VM details and
runtime error bodies. All credential and CI responses disable caching.
Git-managed and controller-owned services reject direct CI changes. Active Stack
work excludes CI admission. Static YAML may reassert its declared image on a
later apply. Automatic tag policy remains active; it shares the same queue and
release checks. CI does not provision public DNS, TLS, tunnels, or registry accounts.

## Acceptance

Run `python3 scripts/test-deploy-ci-client.py` for the Action client contract.
The Installer CI adds `--ci-deploy` to `scripts/ci-qa-deploy.py` on its disposable
Linux/KVM runner. It tests CLI disconnect, API restart, Action resume, failed health,
expiry, revocation and HTTP continuity through private verified TLS and real Nginx.
The runner's `ci.example.test` edge and registry fixture never contact public DNS
or publish an external image. Reports include resource limits and complete cleanup.
