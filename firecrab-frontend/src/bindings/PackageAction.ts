// Mirrors firecrab_api_types::PackageAction (camelCase wire shape).
// `PackageActionKind` is inlined as a literal union rather than exported
// separately — nothing imports the kind on its own.

export type PackageAction = { action: "install" | "remove" | "update", packages: Array<string>, };
