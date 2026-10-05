param(
    [Parameter(Mandatory=$true)][string]$CliPath,
    [string]$ArtifactDir = "target\qa\micromanager-sleepy",
    [string]$Template = "",
    [string]$Subnet = "192.168.251.0/24"
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $ArtifactDir | Out-Null
$checks = [System.Collections.Generic.List[string]]::new()
$base = 'http://127.0.0.1:5523'
$vm = $null; $network = $null; $hold = $null
$original = $null
$result = @{status='FAILED'; checks=$checks; platform='native Windows + WSL2'; tty='not exercised'}
function Assert-That($condition, [string]$name) {
    if (-not $condition) { throw "FAILED: $name" }
    $checks.Add($name); Write-Output "PASS: $name"
}
function Api([string]$method, [string]$path, $body=$null) {
    $args_ = @{Method=$method; Uri="$base$path"; TimeoutSec=360}
    if ($null -ne $body) { $args_.Body = ($body | ConvertTo-Json -Depth 8); $args_.ContentType = 'application/json' }
    Invoke-RestMethod @args_
}
function State { (Api 'GET' '/api/micromanager/status').state }
function Distro-Running {
    ((& wsl.exe --list --running --quiet) -join "`n").Replace([string][char]0, '').Contains('firecrab-debian')
}
try {
    $original = (& $CliPath service settings --json | Out-String | ConvertFrom-Json)
    Assert-That ($LASTEXITCODE -eq 0) 'read original settings'
    $base = "http://127.0.0.1:$($original.api_port)"
    & $CliPath service start
    Assert-That ($LASTEXITCODE -eq 0) 'start before checking guest activity'
    $busy = Api 'GET' '/api/micromanager/activity'
    Assert-That (-not $busy.busy) 'guest is idle before activation'
    & $CliPath service settings --set sleepy=true --set idle_minutes=1 --set autostart=false
    Assert-That ($LASTEXITCODE -eq 0) 'save Sleepy settings'
    & $CliPath service start
    Assert-That ($LASTEXITCODE -eq 0) 'activate resident controller'
    Assert-That ((State) -eq 'running') 'controller reports running'
    $images = @(Api 'GET' '/api/images')
    if (-not $Template) {
        $installed = @($images | Where-Object { $_.installed -and $_.alias.StartsWith('alpine-') })
        if ($installed.Count -eq 0) { throw 'Install an Alpine image or provide -Template before running native QA' }
        $Template = $installed[0].alias
    }
    Assert-That (@($images | Where-Object { $_.installed -and $_.alias -eq $Template }).Count -eq 1) 'test image is installed'

    $hold = [System.Net.Sockets.TcpClient]::new('127.0.0.1',[int]$original.api_port)
    $stream = $hold.GetStream(); $stream.ReadTimeout = 360000
    $request = [Text.Encoding]::ASCII.GetBytes("POST /api/micromanager/hold HTTP/1.1`r`nHost: localhost`r`nContent-Length: 0`r`n`r`n")
    $stream.Write($request,0,$request.Length)
    $header = ''; while (-not $header.EndsWith("`r`n`r`n")) { $byte = $stream.ReadByte(); if ($byte -lt 0) { throw 'hold disconnected' }; $header += [char]$byte }
    Assert-That ($header.StartsWith('HTTP/1.1 200')) 'host shell lease accepted'
    Start-Sleep -Seconds 75
    Assert-That ((State) -eq 'running') 'open shell inhibits idle sleep'
    $hold.Close(); $hold = $null
    Start-Sleep -Seconds 75
    Assert-That ((State) -eq 'sleeping') 'idle timeout transitions to sleeping'
    Assert-That (-not (Distro-Running)) 'managed WSL distribution actually stops'
    $statusCode = 0
    try { Api 'GET' '/api/vms' | Out-Null } catch { $statusCode = [int]$_.Exception.Response.StatusCode }
    Assert-That ($statusCode -eq 503) 'monitoring reports sleeping without waking'
    Start-Sleep -Seconds 5
    Assert-That (-not (Distro-Running)) 'monitoring leaves WSL stopped'

    $name = 'sleepy-' + [guid]::NewGuid().ToString('N').Substring(0,12)
    $network = Api 'POST' '/api/micro-networks' @{name=$name; subnetCidr=$Subnet}
    Assert-That ((State) -eq 'running' -and (Distro-Running)) 'work request automatically boots WSL and completes'
    $vm = Api 'POST' '/api/vms' @{name=$name; template=$Template; ram=256; cpu=1; diskGb=2; microNetworkId=$network.id}
    Api 'POST' "/api/vms/$($vm.id)/start" | Out-Null
    $deadline = (Get-Date).AddMinutes(5)
    do {
        $record = Api 'GET' "/api/vms/$($vm.id)"
        if ($record.state -eq 'error') { throw "VM failed: $($record | ConvertTo-Json -Depth 5)" }
        if ($record.state -eq 'running') { break }
        Start-Sleep -Seconds 2
    } while ((Get-Date) -lt $deadline)
    Assert-That ($record.state -eq 'running') 'real MicroVM starts through the proxy'
    Start-Sleep -Seconds 75
    Assert-That ((State) -eq 'running') 'running MicroVM inhibits idle sleep'
    Api 'POST' "/api/vms/$($vm.id)/stop" | Out-Null
    Api 'DELETE' "/api/vms/$($vm.id)" | Out-Null; $vm = $null
    Api 'DELETE' "/api/micro-networks/$($network.id)" | Out-Null; $network = $null

    & $CliPath service stop
    Assert-That ($LASTEXITCODE -eq 0 -and (State) -eq 'stopped') 'manual stop is distinct from sleeping'
    $statusCode = 0
    try { Api 'POST' '/api/vms' @{name='must-not-wake'} | Out-Null } catch { $statusCode = [int]$_.Exception.Response.StatusCode }
    Assert-That ($statusCode -eq 503 -and -not (Distro-Running)) 'work cannot undo a manual stop'
    & $CliPath service start
    Assert-That ($LASTEXITCODE -eq 0 -and (State) -eq 'running') 'explicit start releases manual stop'
    $alternatePort = if ($original.api_port -eq 5533) { 5534 } else { 5533 }
    & $CliPath service settings --set "api_port=$alternatePort"
    Assert-That ($LASTEXITCODE -eq 0 -and (State) -eq 'running') 'saving a port keeps the activated endpoint working'
    & $CliPath service start
    Assert-That ($LASTEXITCODE -eq 0) 'apply alternate API port'
    $base = "http://127.0.0.1:$alternatePort"
    Assert-That ((State) -eq 'running') 'controller listens at the new port'
    & $CliPath image list --json | Out-Null
    Assert-That ($LASTEXITCODE -eq 0) 'CLI uses the activated API port by default'
    $result.status = 'PASS'
    $result.template = $Template
} catch {
    $result.error = $_.ToString()
    throw
} finally {
    if ($null -ne $hold) { $hold.Close() }
    try {
        if ($null -ne $vm) { Api 'POST' "/api/vms/$($vm.id)/stop" | Out-Null; Api 'DELETE' "/api/vms/$($vm.id)" | Out-Null }
        if ($null -ne $network) { Api 'DELETE' "/api/micro-networks/$($network.id)" | Out-Null }
        if ($null -ne $original) {
            & $CliPath service settings --set "sleepy=$($original.sleepy.ToString().ToLowerInvariant())" --set "idle_minutes=$($original.idle_minutes)" --set "autostart=$($original.autostart.ToString().ToLowerInvariant())" --set "api_port=$($original.api_port)"
            Assert-That ($LASTEXITCODE -eq 0) 'restore original settings after QA'
            & $CliPath service start
            Assert-That ($LASTEXITCODE -eq 0) 'restore activated API port after QA'
        }
    } catch {
        $result.status = 'FAILED'
        $result.cleanupError = $_.ToString()
        Write-Warning $_
    }
    $result | ConvertTo-Json -Depth 5 | Set-Content "$ArtifactDir\summary.json"
}
if ($result.status -ne 'PASS') { exit 1 }
