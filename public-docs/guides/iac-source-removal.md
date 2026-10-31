# Disconnect a Git source safely

Git source configuration uses the exact Stack registration UUID as its `stack_source` identity.
Shared [ownership](iac-ownership.md) protects configuration changes, including identical saves and pause/resume.
An independent native edit cannot replace Terraform-owned configuration.
Controller checks and their observations still operate on the saved configuration.
Active checks or applies block source transfer and removal.

Removing a source preserves its Stack registration, bindings, infrastructure, application data and retained check history.
It stops future polling and returns control to manual Stack apply.
It does not recover an unfinished deployment or release existing resource ownership.

## Remove the observed generation

Read `firecrab iac identity`, `firecrab stack status demo` and `firecrab stack source status demo` on the intended host.
Use the returned endpoint UUID, registration UUID and source generation.
Save this request as `remove-source.json` and replace every placeholder with an actual value:

```json
{
  "operationId": "OPERATION_UUID",
  "expectedEndpointId": "ENDPOINT_UUID",
  "expectedStackId": "STACK_UUID",
  "expectedGeneration": 1
}
```

Generate one non-nil operation UUID before sending the request.
Retain the request file unchanged after a lost response:

```sh
firecrab stack source remove demo --file remove-source.json
firecrab iac native-operation --id OPERATION_UUID
firecrab stack source history demo
```

The CLI sends `DELETE /api/stacks/demo/source` through the normal selected host and mTLS client.
The exact source removal and completed acknowledgement commit in one SQLite transaction.
An identical operation retry returns its original acknowledgement, including after source recreation.
Reusing that UUID for another registration, generation or typed action fails without mutation.
A fresh request with an old generation also fails.

Transfer or release Terraform configuration ownership before an independent native removal.
An active check/apply rejects removal before recording success.
The URL name alone cannot remove a different registration that later reused that name.

## Reconfigure after removal

Generations do not reset when the source is absent.
`GET /api/stacks/demo/source` returns 404 after removal.
The separate generation endpoint retains its deletion generation:

```sh
firecrab stack source generation demo
```

This calls `GET /api/stacks/demo/source/generation`.
For a source first saved at generation 1, removal retains generation 2.
Reconfigure it with `--expected-generation 2`; the new configuration becomes generation 3.
An old removal acknowledgement cannot delete this new configuration.

This is a shared lifecycle prerequisite of issue #548.
It does not establish the pending inventory-wide Stack or Terraform implementation.
