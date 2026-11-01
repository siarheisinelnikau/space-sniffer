# Fetch the exact public source revision required by the sibling path dependency.
$ErrorActionPreference = 'Stop'
$revision = 'fc80a2648754b450f48255d64187cf7b13e18588'
if (-not $env:GITHUB_WORKSPACE -or -not $env:RUNNER_TEMP) {
    throw 'This provisioning script runs inside GitHub Actions.'
}
$workspace = [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)
$destination = Join-Path ([IO.Directory]::GetParent($workspace).FullName) 'rust-auto-updator'
$marker = Join-Path $destination '.source-revision'
if (Test-Path -LiteralPath $destination) {
    if ((Test-Path -LiteralPath $marker) -and (Get-Content -LiteralPath $marker -Raw).Trim() -eq $revision) {
        exit 0
    }
    throw "Refusing to overwrite an existing sibling checkout: $destination"
}
$temporary = Join-Path $env:RUNNER_TEMP ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $temporary | Out-Null
$archive = Join-Path $temporary 'source.zip'
Invoke-WebRequest -Uri "https://github.com/siarheisinelnikau/rust-auto-updator/archive/$revision.zip" -OutFile $archive
Expand-Archive -LiteralPath $archive -DestinationPath $temporary
$source = Join-Path $temporary "rust-auto-updator-$revision"
if (-not (Test-Path -LiteralPath (Join-Path $source 'crates/rust-auto-updator/Cargo.toml'))) {
    throw 'The pinned source archive does not contain the updater crate.'
}
Copy-Item -LiteralPath $source -Destination $destination -Recurse
Set-Content -LiteralPath $marker -Value $revision -Encoding utf8
