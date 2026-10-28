# Deploy

Deploy a digest-pinned image to one existing Firecrab service. Requires Python
3.10 or later. The Action waits for the exact operation and fails on an
unsuccessful deployment, revoked/expired credentials, or timeout.

```yaml
- uses: SteelCrab/firecrab/.github/actions/deploy@main
  with:
    service-url: https://orders.example.com/__firecrab/deploy
    token: ${{ secrets.FIRECRAB_DEPLOY_TOKEN }}
    image: ghcr.io/team/orders@sha256:YOUR_64_CHARACTER_DIGEST
```

Use a reviewed commit ref for production. See the
[CI Deploy guide](../../../public-docs/guides/ci-deploy.md) for setup, retry
identities, optional inputs, outputs, and cleanup requirements.
