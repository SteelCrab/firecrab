# Secrets and PipeSecrets

Open **Secrets** in the web console, or navigate to `#/secrets`.
PipeSecrets is Firecrab's internal secret store.
Use **Internal** to create, edit, reveal, or delete a secret.
Names support up to 512 ASCII characters: letters, numbers, and `_ . / + = @ -`.
A secret value may be empty and may contain multiple lines.
Values are limited to 16384 UTF-8 bytes, with no NUL bytes.
The store supports up to 1000 secrets and 20 external connections.
Secret lists and save responses contain metadata, never values.
**Reveal** shows a value temporarily and hides it after 30 seconds or when the window loses focus.
Edits require the current version, so a stale editor cannot overwrite a newer value.
A service reference prevents deletion of its secret.

## Linked Firecrab account

Store account secrets at `https://console.firecrab.dev/#secrets`.
Connect that account under **Auth** with an integration token that has secrets-read permission.
In the local **Secrets** screen, select **Preview account secrets**.
Review the changes, then select **Import selected** to copy selected names into PipeSecrets.
Existing values require explicit selection before replacement.
The API uses the saved Auth credential; the browser does not receive the credential or preview values.
Imports record **Firecrab Console** as the source.
Later console edits do not change local copies until another import.
Applying a preview checks that the same account is still connected and the token is still authorized.
A disconnected, replaced, expired, or revoked token blocks the import.

## External connections

Use **External connections** to register a store.
Settings and credentials are saved on the Firecrab API host.

| Provider | Settings | Credentials |
| --- | --- | --- |
| Infisical | Base URL, project ID, environment slug, secret path | Access token with secret read and write permissions |
| AWS Secrets Manager | Region, optional name prefix | Access key ID, secret access key, optional session token |
| Firecrab · PipeSecrets | Another instance's base URL | Optional bearer token for its authenticating reverse proxy |

Infisical supports cloud and self-hosted endpoints.
Its default cloud endpoint is `https://us.infisical.com`.
The adapter uses the v4 API and keeps secret references as stored strings.
Hidden values require additional read permission; they are never treated as empty values.
AWS requires `secretsmanager:ListSecrets` and `secretsmanager:GetSecretValue` for previews.
Uploads also need `secretsmanager:CreateSecret` for new names and `secretsmanager:PutSecretValue` for replacements.
Customer-managed AWS KMS keys may require additional KMS permissions.
Only AWS `SecretString` is supported; a JSON string stays one secret value.
An AWS name prefix limits both the preview scope and permitted upload names.
The remote Firecrab instance must support these Secrets endpoints.
Use HTTPS for external connections.
HTTP is accepted only for loopback addresses.
The adapter does not follow redirects or expose raw provider error bodies.
The optional bearer token is forwarded to the configured endpoint; Firecrab's existing API access boundary still applies.
Protect remotely exposed APIs with an authenticating TLS reverse proxy.
Editing a connection leaves omitted credentials unchanged.
Use the optional token's clear checkbox to remove a saved token.

## Import and upload

1. Select **Preview import** on an external connection.
2. Review the added, changed, and unchanged names.
3. Select the changes to apply, then choose **Import selected**.

New values are selected initially.
Existing values require explicit selection before replacement.
Previews contain no secret values and expire after ten minutes.
Import creates local copies in PipeSecrets, with their source and import time.
Later external changes appear only after another import.
Applying an import preserves an existing secret's ID and description.
Deleting a connection preserves imported secrets and their source history.

For uploads, select local values under **Internal**, choose a connection, then select **Preview upload**.
Review the changes and select **Upload selected**.
The API checks local versions, connection versions, and the remote values again before uploading.
Provider writes are separate requests, so an upload is not a transaction across providers or names.
The response reports completed and failed names separately.
If a request is interrupted, preview again to see the actual target values before retrying.
A preview can be applied only once.
Delete operations are never propagated.
Changes made at the external provider during the final writes remain subject to that provider's concurrency behavior.

## Storage and backup

PipeSecrets uses an AES-256-GCM encrypted vault in the API database path with its extension replaced by `.secrets`.
For `data/firecrab.db`, the directory is `data/firecrab.secrets/`.
The directory contains `vault.bin`, `master.key`, and `vault.lock`.
The directory is owner-only (`0700`); files are owner-only (`0600`).
The vault contains values, provider credentials, metadata, and temporary transfer snapshots.
Its total size, including previews, is limited to 32 MiB.
Writes use a file lock and atomic replacement.
Back up both the encrypted vault and its encryption key using protected storage.
Possession of both files allows decryption; this does not protect against a compromised API host.
A missing or invalid key blocks access and is never silently replaced while a vault exists.

## API

All Secrets responses use `Cache-Control: no-store` and the existing JSON error envelope.
The existing body limit, origin checks, and concurrency limit apply.
Secrets requests have a 60-second deadline.

| Method | Path | Result |
| --- | --- | --- |
| GET / POST | `/api/secrets` | List metadata / create a secret |
| PUT / DELETE | `/api/secrets/{id}` | Edit / delete a local secret |
| POST | `/api/secrets/{id}/reveal` | Explicitly read a value |
| POST | `/api/secrets/account/preview` | Preview linked account secrets using saved Auth |
| GET / POST | `/api/secrets/connections` | List connection metadata / create a connection |
| PUT / DELETE | `/api/secrets/connections/{id}` | Edit / remove a connection |
| POST | `/api/secrets/connections/{id}/preview` | Preview an import or upload |
| POST | `/api/secrets/plans/{id}/apply` | Apply selected preview names |

Preview bodies use `{"direction":"import"}` or `{"direction":"export","secretIds":["UUID"]}`.
Apply bodies use `{"names":["DB_PASSWORD"]}`.
Secret and connection updates require their latest `version`.
Manage stored values and transfers through the web console.
Reference local Secret UUIDs from [MicroService](microservices.md#inject-secrets).
This feature does not add Secrets CLI commands. MicroService value rotation is automatic.

## MicroService injection

Create a value in **Secrets**, then copy its UUID into the service's secret references:

```json
{
  "env": {"APP_ENV": "production"},
  "secrets": {"DB_PASSWORD": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"}
}
```

The environment name is the map key. The value is a local PipeSecrets UUID, not
the password or an external provider name. Imported Secrets work in the same way.
Saving keeps only references. A missing Secret or a value over 4096 UTF-8 bytes
or containing NUL rejects deployment with a `secrets` field error before any VM
is created. Empty and multiline values are supported. The vault itself supports
larger values, but the guest environment limit still applies.

When queued work starts, Firecrab resolves all references together and stores one
encrypted deployment snapshot. Every replica uses that snapshot, including after
a VM or API restart.

### Automatic value rotation

Editing or importing a different value automatically redeploys running,
Nginx-backed services that reference it. Firecrab checks for changes every five
seconds and waits for admitted Deploy work to finish. The update keeps the
current image digest, tag declaration, service configuration, domain, and network.
Metadata edits and unchanged values keep the current replicas. Stopped and
unrelated services stay as they are.

Every attempt captures one encrypted Secret environment when its worker starts.
Edits made during an update are reconciled after that attempt finishes. API
restart resumes admitted Deploy work and compares desired values again, so an
in-memory edit notification is not required.

Use the service's Deploy status/log or `firecrab microservice status SERVICE_UUID`
to follow progress and failures. Auth, capacity, readiness, ownership, and verified
drain checks apply to automatic updates. A failed attempt retains or restores the
previous release and its captured values. Invalid references, oversized guest
values, missing private snapshots, or a service without an Nginx domain preserve
the running service and show a status error.
For external credentials shared by both generations, keep the old and new values
valid until the rollout finishes.

Retries use increasing delays and stop after five failed admissions/attempts for
the same values and source release. Fix the reported cause and use Deploy to
retry, or save a different Secret value. Metadata edits do not reset this limit.
A drain failure that retains owned VMs requires the existing Deploy retry/Stop
recovery controls. Rollback history still contains Secret references; selecting
an older release uses the current values of those references.

Secret values stay out of service configuration, deployment records, VM metadata,
logs generated by the deployment controller, and SQLite. On boot they are merged
with plain environment variables only in memory, then written to the owned VM's
`/etc/firecrab/vm.env` with root-only mode `0600`. Host staging files also use
mode `0600` and are removed after use. Guest root and the application can read
their values; applications must avoid printing them to their own logs. Guest
disks contain the injected values and must be treated as sensitive.

Editing a Secret-backed replica's environment through the VM API is rejected.
Edit its MicroService configuration instead. Missing or damaged vault state
blocks a fresh boot; a running guest continues independently. A standalone VM
or a service without Secret references does not need the vault.

Stop and failed-deploy cleanup remove the encrypted snapshot after removing all
owned replicas. If cleanup fails, the snapshot remains available for a retry.
Normal PipeSecrets records remain stored for reuse by other services.

## Verification

Verify shared-Secret rotation, restart recovery, failed-health rollback, and cleanup
on an empty disposable Linux/KVM host with at least four CPUs:

```sh
sudo python3 scripts/ci-qa-deploy.py --disposable-host --secret-rotation \
  --report target/qa/secret-rotation.json
```
