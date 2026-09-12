1..60 | ForEach-Object { (Get-Date -Format "HH:mm:ss") | Out-File -Append -Encoding utf8 "T:\cod\PC\ChyguiSlide 2\_canary.txt"; Start-Sleep -Seconds 2 }
