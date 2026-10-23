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
| GET / POST | `/api/secrets/connections` | List connection metadata / create a connection |
| PUT / DELETE | `/api/secrets/connections/{id}` | Edit / remove a connection |
| POST | `/api/secrets/connections/{id}/preview` | Preview an import or upload |
| POST | `/api/secrets/plans/{id}/apply` | Apply selected preview names |

Preview bodies use `{"direction":"import"}` or `{"direction":"export","secretIds":["UUID"]}`.
Apply bodies use `{"names":["DB_PASSWORD"]}`.
Secret and connection updates require their latest `version`.
Manage stored values and transfers through the web console.
Reference a local Secret UUID from a [MicroService](microservices.md#inject-secrets)
to inject its value into every replica. Each deployment keeps an encrypted
snapshot, so edits and imports take effect on the next deployment. VM restarts
keep the existing snapshot. Service injection is limited to 4096 bytes per value.
This feature does not add Secrets CLI commands or automatic rotation.
