param(
    [string] $InnoCompiler
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$workspaceManifest = Join-Path $repoRoot 'Cargo.toml'
$installerScript = Join-Path $repoRoot 'installer\p2p-client.iss'
$clientExe = Join-Path $repoRoot 'target\release\p2p-client.exe'
$installerOutput = Join-Path $repoRoot 'target\installer'

$manifestText = Get-Content -LiteralPath $workspaceManifest -Raw
$versionMatch = [regex]::Match($manifestText, '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*$')
if (-not $versionMatch.Success) {
    throw 'Não foi possível ler a versão MAJOR.MINOR.PATCH de Cargo.toml.'
}
$version = $versionMatch.Groups[1].Value

if (-not (Get-Command 'cl.exe' -ErrorAction SilentlyContinue)) {
    $msvcCandidates = @(
        (Get-ChildItem -Path "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\*\bin\Hostx64\x64" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName),
        (Get-ChildItem -Path "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\*\bin\Hostx64\x64" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName),
        (Get-ChildItem -Path "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Tools\MSVC\*\bin\Hostx64\x64" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName),
        (Get-ChildItem -Path "C:\Program Files\Microsoft Visual Studio\2022\Professional\VC\Tools\MSVC\*\bin\Hostx64\x64" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName)
    )
    $foundMsvc = $msvcCandidates | Where-Object { $_ -and (Test-Path (Join-Path $_ 'cl.exe') -PathType Leaf) } | Select-Object -First 1
    if ($foundMsvc) {
        $env:PATH = "$foundMsvc;$env:PATH"
    } else {
        throw 'O compilador MSVC não está no PATH. Abra o Developer PowerShell for Visual Studio 2022 e execute este script novamente.'
    }
}

if (-not $env:RC -and -not (Get-Command 'rc.exe' -ErrorAction SilentlyContinue)) {
    $rcCandidate = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\rc.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName | Select-Object -First 1
    if ($rcCandidate) {
        $env:RC = $rcCandidate
    }
}

Push-Location $repoRoot
try {
    & cargo build --release --offline -p p2p-client
    if ($LASTEXITCODE -ne 0) {
        throw "A compilação release falhou (código $LASTEXITCODE)."
    }
}
finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $clientExe -PathType Leaf)) {
    throw "O executável compilado não foi encontrado: $clientExe"
}

if ([string]::IsNullOrWhiteSpace($InnoCompiler)) {
    $command = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($command) {
        $InnoCompiler = $command.Source
    }
    else {
        $candidates = @(
            (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
            (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
            (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
            (Join-Path $env:LOCALAPPDATA 'Programs\Antigravity IDE\resources\app\node_modules\innosetup\bin\ISCC.exe')
        )
        $InnoCompiler = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    }
}

if ([string]::IsNullOrWhiteSpace($InnoCompiler) -or -not (Test-Path -LiteralPath $InnoCompiler -PathType Leaf)) {
    throw 'Inno Setup 6 não foi encontrado. Instale o Inno Setup 6 ou passe seu caminho com -InnoCompiler.'
}

New-Item -ItemType Directory -Path $installerOutput -Force | Out-Null
$compilerArgs = @(
    "/DAppVersion=`"$version`"",
    "/DSourceExe=`"$clientExe`"",
    "/DOutputDir=`"$installerOutput`"",
    $installerScript
)
& $InnoCompiler @compilerArgs
if ($LASTEXITCODE -ne 0) {
    throw "A compilação do instalador falhou (código $LASTEXITCODE)."
}

$setupExe = Join-Path $installerOutput 'P2P-Voz-e-tela-Setup.exe'
if (-not (Test-Path -LiteralPath $setupExe -PathType Leaf)) {
    throw "O instalador não foi encontrado após a compilação: $setupExe"
}

$setupInfo = Get-Item -LiteralPath $setupExe
$setupHash = (Get-FileHash -LiteralPath $setupExe -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "Versão: $version"
Write-Output "Executável: $clientExe"
Write-Output "Instalador: $setupExe"
Write-Output "Tamanho do instalador: $($setupInfo.Length) bytes"
Write-Output "SHA-256 do instalador: $setupHash"
