# API errors

Every API error uses the same JSON shape.
The request ID also appears in the `X-Request-Id` response header.

```json
{
  "error": {
    "code": "validation_failed",
    "message": "request validation failed",
    "fields": {
      "cpu": "must be between 1 and 32"
    },
    "requestId": "<uuid>"
  }
}
```

`fields` is empty when the error does not belong to one input field.
Internal paths and private error details are not returned.

## Common codes

| Code | Status | Meaning |
| --- | --- | --- |
| `validation_failed` | 400 | One or more fields are invalid |
| `invalid_json` | 400 | The body is not one valid JSON object |
| `forbidden_origin` | 403 | The browser origin is not allowed |
| `not_found` | 404 | The resource or route does not exist |
| `invalid_state` | 409 | The VM state blocks the operation |
| `in_use` | 409 | Another resource still depends on this resource |
| `vm_not_running` | 409 | The VM has no active console |
| `unsupported_media_type` | 415 | The content type is not JSON |
| `request_too_large` | 413 | The body is larger than 64 KiB |
| `too_many_requests` | 429 | The request concurrency limit was reached |
| `internal_error` | 500 | The server failed without exposing details |
| `unavailable` | 503 | A required service or setting is missing |
| `request_timeout` | 504 | REST processing took more than 10 seconds |

Image jobs also use specific `409` codes.
Examples include `already_installed`, `package_required`, and `install_in_progress`.

## Debug an error

1. Copy the `requestId` from the response.
2. Find the same ID in the `firecrab-api` log.
3. Fix field errors before retrying.
4. Read [troubleshooting](troubleshooting.md) for runtime failures.
