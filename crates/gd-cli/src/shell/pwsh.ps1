function global:gd {
    $info = @('link','unlink','list','clean','export','init','doctor','version','help','hook','-h','--help','-V','--version')
    if ($args.Count -gt 0 -and $args[0] -in $info) {
        & gd.exe @args
        return
    }
    if ($args.Count -eq 1 -and $args[0] -eq '-') {
        Set-Location -
        return
    }
    $result = (& gd.exe @args)
    if ($LASTEXITCODE -eq 0 -and $result) { Set-Location -Path $result }
}
