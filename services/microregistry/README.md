# Firecrab MicroRegistry

Firecrab-owned Cloudflare Worker that serves the `registry` R2 bucket and its
landing page through `registry.firecrab.dev`.

## Routes

- `/`: static package registry page
- `/_health`: service readiness response
- `/catalog.json`: package catalog from R2
- `/{distro}/{version}/*.tar.zst`: package archive from R2
- `/{distro}/{version}/SHA256SUMS`: checksum from R2

Only those published object types are exposed. Hidden and arbitrary R2 objects
are never returned by the service.

## Development and deployment

```sh
cd services/microregistry
npm install
npm run types
npm run typecheck
npm test
npm run check
npm run deploy
```

The first production deploy attaches the configured custom domain. Remove the
old R2 custom-domain attachment for `registry.firecrab.dev` beforehand.
