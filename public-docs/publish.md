# Publish to Cloudflare R2

Configure an `rclone` S3 remote with provider `Cloudflare`, then publish the
complete two-architecture release. A dry run validates every package and
prints all destination object keys.

```sh
R2_BUCKET=firecrab-registry R2_REMOTE=r2 \
  ./scripts/publish-m2images-r2.sh --dry-run

R2_BUCKET=firecrab-registry R2_REMOTE=r2 \
  ./scripts/publish-m2images-r2.sh
```

Packages are uploaded before `catalog.json`; the catalog is the publication
commit point. The script requires every manifest alias for both architectures
to prevent a partial release from replacing the public catalog. Wrangler v4
is available as a fallback with `--backend wrangler`, but its 315 MB upload
limit makes `rclone` the normal choice for compressed rootfs packages.

This path is the public catalog release.
It does not register a host-local custom alias; see [Images](images.md).

## Related

- [Images](images.md)
- [API](api.md)
- [Operations](operations.md)
