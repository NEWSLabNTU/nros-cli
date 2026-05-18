# packages/codegen/packages/nros-codegen-c/CMakeLists.txt
#
# Builds the nros-codegen binary via Corrosion (Cargo integration).
# The binary is used by NanoRosGenerateInterfaces.cmake to generate
# C bindings from .msg/.srv/.action files.
#
# This file is included from the top-level CMakeLists.txt via add_subdirectory().
# It uses the codegen workspace Cargo.toml (one level up).

corrosion_import_crate(
    MANIFEST_PATH "${CMAKE_CURRENT_SOURCE_DIR}/../Cargo.toml"
    CRATES        nros-codegen-c
    LOCKED
)

# nano-ros Phase 140 — install rules deleted. The superproject
# consumes this submodule via `add_subdirectory(packages/codegen/
# packages/nros-codegen-c)` from the root `CMakeLists.txt`; the
# Corrosion-built `nros-codegen` target is reached by name at
# codegen-invocation time (`$<TARGET_FILE:nros-codegen>` in
# `cmake/NanoRosGenerateInterfaces.cmake`), and the
# `cmake/NanoRosGenerateInterfaces.cmake` module is `include()`d
# directly from the source tree. There is no `cmake --install`
# step + no `<prefix>/bin/nros-codegen` + no `<prefix>/lib/cmake/
# NanoRos/` layout in the nano-ros consumption shape, so the
# previous `install(PROGRAMS ...) + install(FILES ...)` rules
# were dead code.
