# Builds the anicca-engine WASM and installs the glue into ../src/wasm.
# Requires: rustup (stable + wasm32-unknown-unknown target) and wasm-bindgen 0.2.100.

# Note: cargo writes progress to stderr; keep ErrorActionPreference at Continue and
# check $LASTEXITCODE explicitly so normal stderr output is not treated as fatal.
$ErrorActionPreference = "Continue"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Push-Location $PSScriptRoot
try {
    Write-Host "Compiling anicca-engine to wasm32..."
    cargo build --release --target wasm32-unknown-unknown
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $wasm = "target/wasm32-unknown-unknown/release/anicca_engine.wasm"
    $out = "out"
    New-Item -ItemType Directory -Force $out | Out-Null

    Write-Host "Generating bindings..."
    wasm-bindgen $wasm --out-dir $out --target web --out-name anicca-engine

    $dst = "../src/wasm"
    Copy-Item "$out/anicca-engine.js" $dst -Force
    Copy-Item "$out/anicca-engine_bg.wasm" $dst -Force
    Copy-Item "$out/anicca-engine_bg.wasm.d.ts" $dst -Force

    Write-Host "Engine installed to src/wasm. NOTE: anicca-engine.d.ts (rich types) is"
    Write-Host "maintained separately and not overwritten by this script."
    Remove-Item $out -Recurse -Force
}
finally {
    Pop-Location
}
