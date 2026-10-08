# Roost additive DRM commit-cookie APIs

Exact published drm-ffi 0.9.1 sources; original MIT LICENSE and VCS metadata retained.
ROOST-PROVENANCE.json records archive URL/SHA, original VCS and modified-file hashes.
ROOST-COMMIT-COOKIE-PATCH.diff is relative to that original archive, not a new upstream release.

Old API signatures and default values remain: legacy userdata is original CRTC,
atomic userdata is zero. New explicit scalar-u64 methods preserve synchronous
slice/request lifetimes and original ioctl results. Userdata is never a pointer.
No generated bindings or kernel ABI layout changed. New separate bounded events
preserve actual kernel CRTC independently from full-width userdata; old event
structs/parser remain unchanged. TEST_ONLY is not accepted presentation.

The workspace and excluded standalone Smithay each explicitly patch both crates.
Shipping Cargo.lock retains the exact versions and original transitive graph,
with path source entries replacing registry source/checksum only. Full selected
Cargo graphs must be verified in CI; no-deps metadata is not graph qualification.
The published standalone locks are historical upstream dependency closures, not
the shipping graph. CI's ffi unit regressions use its original standalone lock;
standalone Smithay resolves its previously absent lock only in CI, then tests locked.
Actual native callback/lifecycle and production package qualification remain required.

The archive-relative patch uses zero-context hunks; GNU patch --forward --fuzz=0 -p1 reproduces every original file exactly as modified here.
