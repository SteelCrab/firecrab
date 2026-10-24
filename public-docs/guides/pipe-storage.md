# PipeStorage: Amazon S3

PipeStorage connects an existing AWS S3 general-purpose bucket. Open
**Pipe → PipeStorage** to add a connection, verify it, and browse files.
S3 keeps the data. Firecrab stores connection metadata and private credentials
in its existing owner-only SQLite database. Credentials are not application-encrypted.

## Connect a bucket

Choose **Connect bucket**, then enter the bucket, AWS region, and optional folder.
The bucket name is also the display name unless you set a name under
**More options**. A nonempty folder is normalized to end in `/`.
Object keys include that prefix.
For example, a connection limited to `team/` accepts `team/report.csv`
and rejects `other/report.csv` and `team-other/report.csv`.
Keys containing control characters or `.`/`..` path segments are unsupported.
Dotted buckets use regional path-style HTTPS to preserve certificate validation.
S3 Express directory buckets are unsupported.

Choose one credential reference:

- **Auth AWS** (`pipeAuthAws`): reuse the saved AWS signing keys. ECR
  authentication alone does not prove S3 access. The selected S3 region can
  differ from the ECR region.
- **Connection-specific keys** (`connection`): supply an access key ID,
  secret access key, and optional session token. These keys need S3 permissions
  only; ECR access is not required. Editing without replacement keys keeps
  the saved keys. Replacing temporary keys also replaces or clears their token.

Save verifies a one-item listing at the exact configured prefix. It does not
enumerate account buckets, upload a probe, or delete an object. Failed validation
keeps the previous connection unchanged. The verification response reports
`list: allowed`; read, write, and delete remain `unknown` until those operations
are attempted. A successful listing cannot prove every IAM capability.

Select a connection to open its files. Use folders and the breadcrumb trail to
navigate, or **Enter folder path** to open a folder directly. **Load more** keeps
the current files visible while adding the next page. File sizes and modification
times are formatted for the selected language.

Choose **Upload file**, select a file, then review its name and destination before
uploading. Failed uploads keep the selected file and settings for a retry.
Click a file name to download it. Its **⋯** menu also contains download and delete.
The connection's **⋯** menu contains edit, connection check, and disconnect.
Connections used by a MicroService explain why edit and disconnect are disabled.

## AWS permissions

Use an IAM principal limited to the chosen bucket and prefix. This example
allows listing, reading, uploading, and deleting under `team/`:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::example-bucket",
      "Condition": {"StringLike": {"s3:prefix": ["team/", "team/*"]}}
    },
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
      "Resource": "arn:aws:s3:::example-bucket/team/*"
    }
  ]
}
```

For read-only access, omit `s3:PutObject` and `s3:DeleteObject`. Listing and
downloading still work. Uploads and deletes return permission errors.
Registration requires `s3:ListBucket` on the configured prefix.
`s3:ListAllMyBuckets` is not required.
SSE-KMS buckets can require additional permissions on the encryption key.
See [S3 policy actions](https://docs.aws.amazon.com/AmazonS3/latest/userguide/using-with-s3-policy-actions.html)
and [ListObjectsV2](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html).

## Files and transfer limits

Listings return up to 100 objects/directories by default, or 1000 through the API.
Pass the opaque `nextCursor` unchanged with the same prefix to fetch the next page.
Object metadata is limited to 4 MiB per upstream response.

Uploads and downloads stream through the API. Each object is limited to 512 MiB.
At most four object transfers run at once. A download keeps its slot until its
body completes or the client disconnects.
Uploads require an exact `Content-Length`. Transfers have a 120-second deadline;
metadata operations have an eight-second upstream deadline. No multipart upload,
automatic retry, filesystem mount, backup, or restore job is included.

Uploads reject existing keys by default using S3's conditional write.
Explicitly enable **Allow overwrite** or CLI `--overwrite` to replace an object.
The CLI also protects an existing local download destination by default.
An interrupted CLI download never replaces its destination with partial data.
If an upload loses its response, inspect the key before retrying: S3 may have
accepted the write. See [S3 conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html).

Deletion affects one explicit current key. For versioned buckets, S3's normal
delete-marker behavior applies; this interface does not permanently purge versions.
Disconnect removes Firecrab connection state without deleting buckets or objects.
It does not revoke the AWS key itself.

## Bind a MicroService

Select **S3 connection** in the service form, or add its ID to the service JSON:

```json
{
  "name": "reports",
  "image": "ghcr.io/example/reports:1.0",
  "port": 8080,
  "ram": 512,
  "cpu": 1,
  "storageConnectionId": "00000000-0000-0000-0000-000000000001"
}
```

At each boot, Firecrab resolves the deployment's connection reference and supplies
`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_REGION`,
`AWS_DEFAULT_REGION`, `FIRECRAB_S3_BUCKET`, and `FIRECRAB_S3_PREFIX` in the guest's
root-owned `/etc/firecrab/vm.env` with mode `0600`. The image entrypoint receives
the environment through the existing service launcher. Use the AWS SDK's normal
environment credential provider. The app must prepend `FIRECRAB_S3_PREFIX` to its keys.

Credential values do not enter VM metadata, service JSON, deployment snapshots,
or API read responses. The guest and its administrators can access them.
Restrict actual application access with AWS IAM: the connection prefix is enforced
by Firecrab's object API, but cannot restrict an application's direct S3 requests.
Avoid conflicting AWS credentials, profiles, or endpoint variables in the service.
Direct environment edits on a storage-bound replica VM are rejected.

Stop and unbind every referencing service before editing or disconnecting a
storage connection. Auth AWS cannot be disconnected while storage references
it. Shared AWS credential replacement rechecks the saved S3 connections before
saving. Restart/redeploy referencing services to pick up new shared keys; already
running guests retain their previous environment. Stop/start, replacement, and
connection deletion never clean up external application data.

## CLI and REST

Global `--api` and `--host` select the host for all storage commands:

```sh
firecrab --host lab pipe storage list
firecrab --host lab pipe storage create --file connection.json
firecrab --host lab pipe storage get CONNECTION_ID
firecrab --host lab pipe storage update CONNECTION_ID --file connection.json
firecrab --host lab pipe storage verify CONNECTION_ID
firecrab --host lab pipe storage objects CONNECTION_ID --prefix team/
firecrab --host lab pipe storage upload CONNECTION_ID team/report.csv report.csv
firecrab --host lab pipe storage download CONNECTION_ID team/report.csv downloaded.csv
firecrab --host lab pipe storage remove-object CONNECTION_ID team/report.csv --yes
firecrab --host lab pipe storage delete CONNECTION_ID
```

A configuration using Auth contains only a reference:

```json
{
  "name": "Reports",
  "bucket": "example-bucket",
  "region": "ap-northeast-2",
  "prefix": "team/",
  "credentialRef": "pipeAuthAws"
}
```

For connection-specific keys, use `credentialRef: "connection"` and an input-only
`credentials` object with `accessKeyId`, `secretAccessKey`, and `sessionToken`.
Keep such input files private and remove them after submission. Do not commit them.

| Method and path | Operation |
| --- | --- |
| `GET /api/pipe/storage` | List secret-free connections and service references |
| `POST /api/pipe/storage` | Verify and create |
| `GET /api/pipe/storage/{id}` | Inspect one connection |
| `PUT /api/pipe/storage/{id}` | Verify and replace unbound connection settings |
| `DELETE /api/pipe/storage/{id}` | Disconnect an unbound connection |
| `POST /api/pipe/storage/{id}/verify` | Non-mutating permission check |
| `GET /api/pipe/storage/{id}/objects?prefix=...&cursor=...&limit=100` | List a directory page |
| `GET /api/pipe/storage/{id}/object?key=...` | Download one key |
| `PUT /api/pipe/storage/{id}/object?key=...&overwrite=false` | Upload a sized binary body |
| `DELETE /api/pipe/storage/{id}/object?key=...` | Delete one key |

URL-encode keys and continuation tokens. Provider error bodies and credentials
are omitted from errors. Permission, expiry, missing-object/bucket, region,
conflict, and timeout failures have distinct error codes.

This implements the initial AWS S3 provider tracked by
[#439](https://github.com/SteelCrab/firecrab/issues/439), under PipeStorage in
[#334](https://github.com/SteelCrab/firecrab/issues/334).
MinIO, R2, custom endpoints, and automatic backups need separate work.
See [PipeStorage QA](../development/pipe-storage-qa.md) for validation and cleanup.
