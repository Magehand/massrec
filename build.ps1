# Build MASSCAP on Windows with the ASIO input enabled.
# Prerequisites: Rust (rustup), LLVM (winget install LLVM.LLVM or choco install llvm).
# The Steinberg ASIO SDK is downloaded by asio-sys at build time; set CPAL_ASIO_DIR
# to use a local copy instead.
$ErrorActionPreference = "Stop"
if (-not $env:LIBCLANG_PATH) {
    $candidates = @("C:\Program Files\LLVM\bin", "$env:LOCALAPPDATA\Programs\LLVM\bin")
    foreach ($c in $candidates) { if (Test-Path "$c\libclang.dll") { $env:LIBCLANG_PATH = $c; break } }
}
if (-not $env:LIBCLANG_PATH) { Write-Warning "LIBCLANG_PATH not set and LLVM not found; asio-sys bindgen will fail." }
cargo build --release --workspace --features masscap-cli/asio,masscap/asio
Write-Host "Binaries: target\release\masscap.exe, target\release\masscap-cli.exe"
