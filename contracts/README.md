# Observer-client contract import

The solstone journal owns the observer-client contract. solstone-linux keeps a frozen, byte-exact copy of bundle version 8.0.0 in `vendor/observer-client-contract/` so this observer can verify the agreement fully offline while experiencing the desktop along with its owner.

To adopt an authority revision:

1. Export the bundle from the verified authority revision in `https://github.com/solpbc/solstone-journal`.
2. Verify the exported manifest, file inventory, and byte digests, then freeze those exact bytes locally without reformatting or regeneration.
3. Replace `vendor/observer-client-contract/` with the verified export.
4. Update `observer-client-import.json` with the public authority revision, bundle version, manifest path and digest, and vendored root.
5. Update independently pinned conformance expectations only when the authority intentionally changes them.
6. Run `make check-observer-contract` to verify provenance, frozen bytes, identities, and Linux production behavior offline.

Bundle versions, solstone-linux application releases, and observer wire-protocol versions evolve independently. If conformance exposes an incompatibility, resolve it against the journal authority before changing consumer behavior. Never edit authority fixtures or vectors to fit the observer.

## Rust release manifest schema import

solstone-linux keeps a frozen, byte-exact copy of the Rust release manifest schema in `vendor/rust-release-manifest/`. Its public schema ID and embedded SHA-256 digest are the provider-neutral authority. The repository-local renderer and validator use that copy to produce and verify release evidence fully offline.

To adopt an authority revision, verify the schema ID, dialect, byte length, and SHA-256 before replacing the vendored file. Then update `rust-release-manifest-import.json` with that exact provider-neutral schema provenance. Do not reformat or regenerate the schema during import. Run `make check-rust-release-manifest` to verify the frozen schema, renderer, checksums, package metadata, and release-directory classifier offline.
