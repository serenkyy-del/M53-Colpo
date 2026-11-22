# Local Windows compatibility patch

Upstream: gpui-pre-windows 0.3.9, Zed revision recorded in Cargo.toml.
License: Apache-2.0, preserved in LICENSE-APACHE.

Hardware adapter enumeration now falls back to the Windows WARP software adapter
when no hardware device satisfies GPUI requirements. Device recovery uses the same
path. Feature level 11 devices do not require the optional D3D10.x compute flag;
that flag is checked only for the supported feature level 10.1 path.

No shared Cargo registry files are modified. The application pins this copy using
patch.crates-io. Cross-compilation verifies API types and linkage; Windows runtime
verification remains required on the affected workstation.

Development shader compilation now calls D3DCompile on include_str! sources,
with the shared alpha_correction.hlsl include expanded in memory. The old
D3DCompileFromFile/canonicalize(CARGO_MANIFEST_DIR) path referenced the Linux
build host and failed with os error 3 on Windows. Packaging checks all three
embedded sources and the PE compiler imports, rejecting this regression.
