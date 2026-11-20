param(
    [switch]$All,
    [switch]$Index,
    [switch]$SelfTest,
    [string]$Root = (Get-Location).Path,
    [string]$ExceptionsPath = (Join-Path $PSScriptRoot 'file-size-exceptions.txt')
)

$ErrorActionPreference = 'Stop'

$DefaultLimit = 1000
$HardLimit = 1200

function Get-RepoRoot([string]$Path) {
    $resolved = & git -C $Path rev-parse --show-toplevel 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $resolved) {
        throw "Not a Git work tree: $Path"
    }
    return $resolved.Trim()
}

function Get-LineCount([byte[]]$Bytes) {
    if ($null -eq $Bytes -or $Bytes.Length -eq 0) {
        return 0
    }
    $count = 0
    foreach ($byte in $Bytes) {
        if ($byte -eq 10) {
            $count++
        }
    }
    if ($Bytes[$Bytes.Length - 1] -ne 10) {
        $count++
    }
    return $count
}

function Test-Binary([byte[]]$Bytes) {
    if ($null -eq $Bytes) {
        return $false
    }
    foreach ($byte in $Bytes) {
        if ($byte -eq 0) {
            return $true
        }
    }
    return $false
}

function Get-IndexBytes([string]$RepoRoot, [string]$Path) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = 'git'
    $info.Arguments = "-C `"$RepoRoot`" cat-file blob `":$Path`""
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.CreateNoWindow = $true
    $process = [System.Diagnostics.Process]::Start($info)
    $memory = New-Object System.IO.MemoryStream
    $process.StandardOutput.BaseStream.CopyTo($memory)
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        throw "git cat-file failed for ${Path}: $stderr"
    }
    return $memory.ToArray()
}

function Get-ExceptionEntries([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return @()
    }
    $entries = @()
    $lineNumber = 0
    foreach ($line in [System.IO.File]::ReadAllLines($Path, [System.Text.Encoding]::UTF8)) {
        $lineNumber++
        $trimmed = $line.Trim()
        if (-not $trimmed -or $trimmed.StartsWith('#')) {
            continue
        }
        $parts = $trimmed.Split('|')
        if ($parts.Count -lt 3) {
            throw "Invalid exception at ${Path}:$lineNumber (expected path|limit|reason)"
        }
        $limit = 0
        if (-not [int]::TryParse($parts[1].Trim(), [ref]$limit)) {
            throw "Invalid exception limit at ${Path}:$lineNumber"
        }
        if ($limit -gt $HardLimit) {
            throw "Exception at ${Path}:$lineNumber exceeds the hard limit $HardLimit"
        }
        $entries += [pscustomobject]@{
            Pattern = $parts[0].Trim()
            Limit = $limit
            Reason = ($parts[2..($parts.Count - 1)] -join '|').Trim()
        }
    }
    return $entries
}

function Get-LimitFor([string]$Path, $Exceptions) {
    foreach ($entry in $Exceptions) {
        if ($Path -like $entry.Pattern) {
            return $entry
        }
    }
    return [pscustomobject]@{
        Pattern = $Path
        Limit = $DefaultLimit
        Reason = ''
    }
}

function Test-Excluded([string]$Path) {
    $patterns = @(
        '.git/*',
        'target/*',
        'bin/*',
        'assets/*',
        'Cargo.lock',
        '*.lock',
        '*.exe',
        '*.dll',
        '*.pdb',
        '*.zip',
        '*.png',
        '*.jpg',
        '*.jpeg',
        '*.gif',
        '*.ico',
        '*.svg',
        '*.ttf',
        '*.otf',
        '*.woff',
        '*.woff2',
        '*.pdf'
    )
    foreach ($pattern in $patterns) {
        if ($Path -like $pattern) {
            return $true
        }
    }
    return $false
}

function Get-TrackedFiles([string]$RepoRoot) {
    $output = & git -C $RepoRoot ls-files -z
    if ($LASTEXITCODE -ne 0) {
        throw 'git ls-files failed'
    }
    return @($output -split "`0" | Where-Object { $_ })
}

function Get-StagedFiles([string]$RepoRoot) {
    $output = & git -C $RepoRoot diff --cached --name-only --diff-filter=ACMR -z
    if ($LASTEXITCODE -ne 0) {
        throw 'git diff --cached failed'
    }
    return @($output -split "`0" | Where-Object { $_ })
}

function Find-GlobalSuppressions([string]$Text) {
    $found = @()
    $number = 0
    foreach ($line in $Text -split "`n") {
        $number++
        if ($line -match '^\s*#!\[\s*allow\s*\(\s*(warnings|clippy::all)\s*\)\s*\]') {
            $found += $number
        }
    }
    return $found
}

function Find-StagedGlobalSuppressions([string]$RepoRoot) {
    $diff = & git -C $RepoRoot diff --cached --unified=0 -- '*.rs'
    if ($LASTEXITCODE -ne 0) {
        throw 'git diff --cached --unified=0 failed'
    }
    $found = @()
    $current = ''
    $lineNumber = 0
    foreach ($line in ($diff -split "`n")) {
        if ($line -match '^\+\+\+ b/(.+)$') {
            $current = $Matches[1]
        } elseif ($line -match '^@@ .* \+(\d+)') {
            $lineNumber = [int]$Matches[1] - 1
        } elseif ($line.StartsWith('+')) {
            $lineNumber++
            if ($line -match '^\+.*#!\[\s*allow\s*\(\s*(warnings|clippy::all)\s*\)\s*\]') {
                $found += "$current`:$lineNumber"
            }
        } elseif (-not $line.StartsWith('-') -and -not $line.StartsWith('\')) {
            $lineNumber++
        }
    }
    return $found
}

function Invoke-SizeCheck([string]$RepoRoot, [switch]$FromIndex, [string]$ExceptionsFile) {
    $exceptions = Get-ExceptionEntries $ExceptionsFile
    $errors = New-Object System.Collections.Generic.List[string]

    if ($FromIndex) {
        $check = & git -C $RepoRoot diff --cached --check 2>&1
        if ($LASTEXITCODE -ne 0 -or $check) {
            $errors.Add('git diff --cached --check failed: whitespace errors or conflict markers')
            foreach ($line in @($check)) {
                if ($line) {
                    $errors.Add("  $line")
                }
            }
        }
        foreach ($entry in (Find-StagedGlobalSuppressions $RepoRoot)) {
            $errors.Add("${entry}: global lint suppression is not allowed")
        }
        $files = Get-StagedFiles $RepoRoot
    } else {
        $files = Get-TrackedFiles $RepoRoot
    }

    foreach ($path in $files) {
        $path = $path.Replace('\', '/')
        if (Test-Excluded $path) {
            continue
        }
        if ($FromIndex) {
            $bytes = Get-IndexBytes $RepoRoot $path
        } else {
            $full = Join-Path $RepoRoot $path
            if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
                continue
            }
            $bytes = [System.IO.File]::ReadAllBytes($full)
        }
        if (Test-Binary $bytes) {
            continue
        }
        $limit = Get-LimitFor $path $exceptions
        $count = Get-LineCount $bytes
        if ($count -gt $limit.Limit) {
            $errors.Add("$path`: $count lines (allowed $($limit.Limit))")
        }
        if (-not $FromIndex -and $path.EndsWith('.rs')) {
            $text = [System.Text.Encoding]::UTF8.GetString($bytes)
            foreach ($line in (Find-GlobalSuppressions $text)) {
                $errors.Add("$path`:$line`: global lint suppression is not allowed")
            }
        }
    }

    if ($errors.Count -gt 0) {
        Write-Host 'File size check failed:'
        foreach ($error in $errors) {
            Write-Host "  $error"
        }
        return $false
    }
    Write-Host "File size check passed ($($files.Count) tracked files, limit $DefaultLimit/$HardLimit)."
    return $true
}

function Invoke-SelfTest {
    $hostExe = (Get-Process -Id $PID).Path
    $tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("voxhold-size-" + [guid]::NewGuid().ToString('N'))
    $tempExceptions = Join-Path $tempRoot 'exceptions.txt'
    New-Item -ItemType Directory -Force $tempRoot | Out-Null

    function Write-Lines([string]$Path, [int]$Count, [string]$Eol) {
        $builder = New-Object System.Text.StringBuilder
        for ($i = 0; $i -lt $Count; $i++) {
            [void]$builder.Append("line $i")
            [void]$builder.Append($Eol)
        }
        [System.IO.File]::WriteAllText($Path, $builder.ToString(), (New-Object System.Text.UTF8Encoding($false)))
    }

    function New-TestRepo([string]$Name) {
        $path = Join-Path $tempRoot $Name
        New-Item -ItemType Directory -Force $path | Out-Null
        & git -C $path init -q
        & git -C $path config user.email 'self-test@example.invalid'
        & git -C $path config user.name 'Self Test'
        return $path
    }

    function Invoke-Child([string]$Repo, [switch]$IndexMode) {
        $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, '-Root', $Repo, '-ExceptionsPath', $tempExceptions)
        if ($IndexMode) {
            $arguments += '-Index'
        } else {
            $arguments += '-All'
        }
        $output = & $hostExe @arguments 2>&1
        return [pscustomobject]@{
            Code = $LASTEXITCODE
            Output = ($output -join "`n")
        }
    }

    $failed = $false
    try {
        [System.IO.File]::WriteAllText($tempExceptions, "c-1200.txt|1200|boundary fixture`nd-1201.txt|1200|hard limit fixture`n", (New-Object System.Text.UTF8Encoding($false)))

        $repo = New-TestRepo 'boundaries'
        Write-Lines (Join-Path $repo 'a-1000.txt') 1000 "`n"
        Write-Lines (Join-Path $repo 'b-1001.txt') 1001 "`n"
        Write-Lines (Join-Path $repo 'c-1200.txt') 1200 "`n"
        Write-Lines (Join-Path $repo 'd-1201.txt') 1201 "`r`n"
        & git -C $repo add -A | Out-Null
        $result = Invoke-Child $repo -IndexMode
        if ($result.Code -eq 0) { throw '1001/1201 staged files must fail' }
        if ($result.Output -notmatch 'b-1001\.txt: 1001 lines \(allowed 1000\)') { throw '1001 error text is missing' }
        if ($result.Output -notmatch 'd-1201\.txt: 1201 lines \(allowed 1200\)') { throw '1201 error text is missing' }

        $repo = New-TestRepo 'passing'
        Write-Lines (Join-Path $repo 'a-1000.txt') 1000 "`n"
        Write-Lines (Join-Path $repo 'c-1200.txt') 1200 "`n"
        Write-Lines (Join-Path $repo 'e-1000-crlf.txt') 1000 "`r`n"
        & git -C $repo add -A | Out-Null
        $result = Invoke-Child $repo -IndexMode
        if ($result.Code -ne 0) { throw "1000/1200 boundaries must pass: $($result.Output)" }

        $repo = New-TestRepo 'partial'
        $partial = Join-Path $repo 'partial.txt'
        Write-Lines $partial 1001 "`n"
        & git -C $repo add -A | Out-Null
        Write-Lines $partial 10 "`n"
        $result = Invoke-Child $repo -IndexMode
        if ($result.Code -eq 0) { throw 'the staged 1001-line version must be checked, not the working tree' }
        if ($result.Output -notmatch 'partial\.txt: 1001 lines \(allowed 1000\)') { throw 'partial-stage error text is missing' }
        $result = Invoke-Child $repo
        if ($result.Code -ne 0) { throw "the 10-line working tree must pass -All: $($result.Output)" }

        Write-Host 'File size self-test passed (1000, 1001, 1200, 1201, CRLF/LF, partially staged).'
    } catch {
        $failed = $true
        Write-Host "File size self-test failed: $($_.Exception.Message)"
    } finally {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($failed) {
        return $false
    }
    return $true
}

if ($SelfTest) {
    if (-not (Invoke-SelfTest)) {
        exit 1
    }
    exit 0
}

if ($Index -and $All) {
    Write-Error 'Use either -Index or -All, not both.'
    exit 1
}
if (-not $Index -and -not $All) {
    $All = $true
}

$repoRoot = Get-RepoRoot $Root
$ok = if ($Index) {
    Invoke-SizeCheck $repoRoot -FromIndex -ExceptionsFile $ExceptionsPath
} else {
    Invoke-SizeCheck $repoRoot -ExceptionsFile $ExceptionsPath
}
if (-not $ok) {
    exit 1
}
