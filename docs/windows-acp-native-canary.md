# Windows ACP native canary

This is a non-release, no-provider canary for the two Windows binaries built from the
`spike/windows-acp-native-agent` branch.

## Safety boundary

- Use only the `openab.exe` and `openab-agent.exe` shipped in the same canary archive.
- Verify the archive and binary SHA-256 values against `MANIFEST.json` and `SHA256SUMS.txt`.
- The script creates an isolated temporary home and uses a non-secret sentinel credential.
- All provider HTTP(S) proxy variables point to the closed loopback port `127.0.0.1:9`.
- It does not log in, read an existing auth store, call a usable provider credential, publish a
  release, install anything, or modify the repository.
- Temporary files are deleted in `finally`. The script force-stops any canary process left after a
  failed assertion.

## Run on Windows 11

Open PowerShell in the extracted directory and run:

```powershell
Get-FileHash .\openab.exe -Algorithm SHA256
Get-FileHash .\openab-agent.exe -Algorithm SHA256

PowerShell -NoProfile -ExecutionPolicy Bypass -File .\windows-acp-native-canary.ps1 `
  -OpenAbExe .\openab.exe `
  -AgentExe .\openab-agent.exe `
  -WorkDir $PWD
```

Success ends with `WINDOWS_DUAL_LAYER_CANARY_PASS`. Any missing response, timeout, unexpected
process count, or surviving downstream agent fails closed with a non-zero exit.

## What this proves

1. Both PE binaries start on Windows.
2. `openab-agent.exe` completes ACP `initialize` and `session/new` over stdio without a provider
   request.
3. `openab.exe` serves loopback `/acp`, completes upstream `initialize` and `session/new`, and the
   first prompt crosses the full `openab.exe -> openab-agent.exe` process boundary.
4. Gateway `session/cancel` settles the upstream waiter as `cancelled`.
5. The CI-only controller regression terminates the Job Object while the connection mutex is held,
   proving cleanup does not depend on the streaming future unwinding or on implicit connection
   drop.
6. Abruptly terminating the root process closes its `KILL_ON_JOB_CLOSE` fallback and removes the
   downstream agent.

It does **not** prove a real model/provider turn, installation UX, Windows 10 compatibility, or
that gateway cancellation stops downstream model computation. Those remain separate gates.
