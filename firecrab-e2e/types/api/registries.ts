// API fixtures for browser tests. No application runtime code.
export interface RegistryConnection { authProvider?: "aws" | "github"; registry: string; username: string; provider: "dockerHub" | "ghcr" | "ecr" | "harbor"; accountName?: string }

export interface RegistryImage { id: string; reference: string; digest: string; architecture: string; sizeBytes: number; templateAlias: string }

export interface RegistryTags { tags: string[]; nextLast: string | null }

export interface RegistryRepositories { nextCursor?: string | null; repositories: string[]; nextPage: number | null; listingSupported: boolean }

export type RegistryTransfer = { action: "pull"; reference: string };
