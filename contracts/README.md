# Observer-client contract import

The solstone journal owns the observer-client contract. solstone-linux keeps a frozen, byte-exact copy of bundle version 1.0.2 in `vendor/observer-client-contract/` so this observer can verify the agreement fully offline while experiencing the desktop along with its owner.

To adopt an authority revision:

1. Export the bundle from the verified authority revision in `https://github.com/solpbc/solstone-journal`.
2. Verify the exported manifest, file inventory, and byte digests, then freeze those exact bytes locally without reformatting or regeneration.
3. Replace `vendor/observer-client-contract/` with the verified export.
4. Update `observer-client-import.json` with the public authority revision, bundle version, manifest path and digest, and vendored root.
5. Update independently pinned conformance expectations only when the authority intentionally changes them.
6. Run `make check-observer-contract` to verify provenance, frozen bytes, identities, and Linux production behavior offline.

Bundle versions, solstone-linux application releases, and observer wire-protocol versions evolve independently. If conformance exposes an incompatibility, resolve it against the journal authority before changing consumer behavior. Never edit authority fixtures or vectors to fit the observer.
