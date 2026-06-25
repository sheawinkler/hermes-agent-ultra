# Normalize HTTPS_PROXY / HTTP_PROXY for git, cmake, and Invoke-WebRequest.

function Get-TalkProxy {
    if ($env:HTTPS_PROXY) { return $env:HTTPS_PROXY.Trim() }
    if ($env:https_proxy) { return $env:https_proxy.Trim() }
    if ($env:HTTP_PROXY) { return $env:HTTP_PROXY.Trim() }
    if ($env:http_proxy) { return $env:http_proxy.Trim() }
    return $null
}

function Initialize-TalkProxy {
    $proxy = Get-TalkProxy
    if (-not $proxy) {
        return $null
    }

    $env:HTTPS_PROXY = $proxy
    $env:HTTP_PROXY = $proxy
    $env:https_proxy = $proxy
    $env:http_proxy = $proxy
    $env:ALL_PROXY = $proxy

    Write-Host "HTTPS_PROXY=$proxy"
    return $proxy
}

function Get-GitProxyArgs {
    param([string]$Proxy = (Get-TalkProxy))
    if (-not $Proxy) {
        return @()
    }
    return @("-c", "http.proxy=$Proxy", "-c", "https.proxy=$Proxy")
}

function Invoke-TalkWebRequest {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Uri,
        [Parameter(Mandatory = $true)]
        [string]$OutFile
    )
    $params = @{
        Uri             = $Uri
        OutFile         = $OutFile
        UseBasicParsing = $true
    }
    $proxy = Get-TalkProxy
    if ($proxy) {
        $params.Proxy = $proxy
    }
    Invoke-WebRequest @params
}
