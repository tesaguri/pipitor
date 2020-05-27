# This script takes care of packaging the build artifacts that will go in the
# release zipfile.

$PKG_NAME = "pipitor"
$SRC_DIR = $PWD.Path
$STAGE = [System.Guid]::NewGuid().ToString()

Set-Location $ENV:Temp
New-Item -Type Directory -Name $STAGE
Set-Location $STAGE

cargo rustc --bin $PKG_NAME --target $Env:TARGET --release --no-default-features --features 'rustls sqlite-bundled' -- -C lto -C codegen-units=1
$ZIP = "$SRC_DIR\$($Env:CRATE_NAME)-$($Env:APPVEYOR_REPO_TAG_NAME)-$($Env:TARGET).zip"

Copy-Item "$SRC_DIR\target\$($Env:TARGET)\release\$PKG_NAME.exe" '.\'

7z a "$ZIP" *

Push-AppveyorArtifact "$ZIP"

Remove-Item *.* -Force
Set-Location ..
Remove-Item $STAGE
Set-Location $SRC_DIR
