#!/usr/bin/env bash
# Builds shinatama-moli.exe on Windows with the tools this machine has:
# CMake and Ninja from the VS Build Tools, aws-lc's prebuilt NASM objects,
# clang-cl and libclang from the LLVM release archive (LLVM_HOME). Cargo's cache stays on D:.
set -euo pipefail
cd "$(dirname "$0")/.."
VS="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/Common7/IDE/CommonExtensions/Microsoft/CMake"
LLVM_HOME="${LLVM_HOME:-/d/Projects/.llvm/clang+llvm-23.1.5-x86_64-pc-windows-msvc}"
export PATH="$LLVM_HOME/bin:$VS/CMake/bin:$VS/Ninja:$PATH"
export CARGO_HOME="${CARGO_HOME_MOLI:-/d/Projects/.cargo-moli}"
export AWS_LC_SYS_PREBUILT_NASM=1
export LIBCLANG_PATH="${LIBCLANG_PATH:-$(cygpath -m "$LLVM_HOME/bin")}"
# Moli tunes codegen-units = 1; on a machine with little free commit, more units keep each
# LLVM module small (CODEGEN_UNITS=1 restores the upstream setting).
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CODEGEN_UNITS:-16}"
export CMAKE_BUILD_PARALLEL_LEVEL="${JOBS:-2}"
exec cargo "${@:-build}" --release -j"${JOBS:-2}" -p shinatama-moli-host
