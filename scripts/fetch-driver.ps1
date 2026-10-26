# Downloads the signed Virtual Display Driver (VirtualDrivers/Virtual-Display-Driver, MIT) into vendor/vdd.
# The checksum is pinned so a changed download fails the build instead of shipping.
$ErrorActionPreference = 'Stop'
$release = '25.7.23'
$url = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/$release/VirtualDisplayDriver-x86.Driver.Only.zip"
$sha256 = 'E24210692B442B39AF763536330CE78B423F19342B7A7792C26DE3944E418B3A'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'vendor\vdd'
$work = Join-Path ([IO.Path]::GetTempPath()) "xiro-vdd-$([Guid]::NewGuid())"
New-Item -ItemType Directory -Force $work, $out | Out-Null
try {
    $zip = Join-Path $work 'vdd.zip'
    Invoke-WebRequest -UseBasicParsing $url -OutFile $zip
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash
    if ($actual -ne $sha256) { throw "Checksum mismatch for $url`n  expected $sha256`n  got      $actual" }
    Expand-Archive $zip -DestinationPath $work -Force
    foreach ($name in 'MttVDD.inf', 'MttVDD.dll', 'mttvdd.cat', 'vdd_settings.xml') {
        $file = Get-ChildItem $work -Recurse -Filter $name | Select-Object -First 1
        if (-not $file) { throw "$name is missing from the driver package" }
        Copy-Item $file.FullName (Join-Path $out $name) -Force
    }
    Write-Host "Virtual Display Driver $release ready in $out"
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
