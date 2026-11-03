# Shared plans for AI model defaults

The `ai_model` configuration adapter uses the [shared plan API](iac-resource-plans.md).
It covers the project-scoped connection/model choice from the v0.4.0 inventory.
Both native Stack and Terraform ownership use the same server implementation.
The complete YAML expansion and typed HCL provider remain pending in #548.

## Configuration and identity

Set the plan request's `desired` to:

```json
{
  "kind": "ai_model",
  "configuration": {
    "projectId": "11111111-1111-4111-8111-111111111111",
    "selection": {
      "connectionId": "22222222-2222-4222-8222-222222222222",
      "model": "YOUR_VERIFIED_MODEL_ID"
    }
  }
}
```

The exact resource identity is `ai_model/PROJECT_UUID`.
This is the existing logical project scope. A model default does not create a
Sandbox project, run inference or grant access to an unverified provider model.
The selected OpenAI connection must already record successful model verification.
Initial account consent and model verification retain their existing API rules.
The declaration contains references and model metadata, never account credentials.

A null resource requests a new default at that exact project identity.
It rejects an existing default, including one with identical values.
For an existing default, supply its exact resource identity after explicit adoption.
Name matching and identical desired values never adopt another configuration.

## Original connection proof

A plan captures the connection UUID, endpoint, owner generation and retained
configuration generation. The public dependency projection contains those
non-secret identities. It omits credentials and credential fingerprints.

Credential or configuration changes invalidate older plans, even if their public
connection metadata remains identical. Disconnect retains a version tombstone.
Recreating the same UUID cannot make an older plan valid again.
Legacy connections receive one initial generation without changing their bytes.

Before execution, the API reads the same retained source version for any normal
OAuth renewal. Refresh can update access/refresh tokens only for the original
client and subject. It preserves the desired configuration generation.
A late refresh cannot modify a disconnected or recreated connection, including
when the replacement has exactly the same private credential document.
The native AI and Sandbox refresh paths enforce the same version comparison.
A refresh failure leaves the model default unchanged and returns a static error.
It never reconnects another account to satisfy an older plan.

## Update, no-op and removal

An unchanged selection produces a no-op. Changing the connection or verified
model updates the same project-scoped resource. The execution transaction
rechecks the exact original selection and dependency generation before commit.
Concurrent configuration changes require a new plan.

Set `desired` to null to remove only the exact owned default.
Removal needs no live provider credential and does not delete the connection,
project, provider account, prompts, inference results or application data.
A borrowed connection keeps its original owner.
Independent model ownership also prevents parent disconnection from silently
removing the managed default.

Completion receipts remain readable after later default removal, connection
removal, expiry and API restart. Repeating a completed apply never reads deleted
credentials, renews a token or recreates the default.

The tests use local SQLite records and private credential fixtures. They verify
reference versions, original identity, refresh races and metadata lifecycle.
They do not establish live provider access or complete both-workflow acceptance.
Other configuration adapters and credential/delegation contracts remain open.
Do not merge #548 until the entire implementation and verification are complete.
