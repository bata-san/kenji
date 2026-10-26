# Windows Service deployment (Kenji + ngrok)

Kenji and ngrok run as **separate native Windows services** so they survive sign-out and restart on boot. The PowerShell installer handles initial registration, SCM automatic start, and recovery actions. It does not copy ngrok credentials into Kenji configuration.

## Prerequisites

- Windows 10/11; start a **locally elevated** Windows PowerShell 5.1 session.
- Build Kenji from `server/` and `web/`, or use a trusted release `kenji.exe`.
- Install ngrok's official CLI from https://ngrok.com/download/windows.
- Create a working **ngrok v3** config with an agent authtoken and an endpoint whose upstream points to Kenji's configured loopback listener (default `http://127.0.0.1:8787`). Use an existing reserved ngrok domain if you have one.
- Store the ngrok YAML somewhere that the **ngrok service account** can read, e.g. `C:\ProgramData\kenji\ngrok.yml`. Restrict its ACL to trusted local principals: the YAML includes a secret. Never commit it to GitHub.

Example config (replace credentials and domain with *your own* existing ones):

```yaml
version: 3
agent:
  authtoken: YOUR_NGROK_AUTHTOKEN
endpoints:
  - name: kenji
    url: https://YOUR_DOMAIN.ngrok-free.app
    upstream:
      url: http://127.0.0.1:8787
```

## Install both services

From the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-windows-services.ps1 -NgrokConfig "C:\ProgramData\kenji\ngrok.yml" -KenjiExe ".\target\release\kenji.exe" -AllowLocalSystem
Get-Service kenji,ngrok
```

Use `-NgrokExe "C:\path\to\ngrok.exe"` if the ngrok CLI is not on PATH, `-InstallDir` to override the Kenji installation directory, or `-NoStart` to register without starting. `-AllowLocalSystem` explicitly opts in to creating a new SYSTEM-privileged Kenji service. For production, instead install the existing Kenji service with `kenji service install --account ...` under a dedicated low-privilege account first, then run this script without that switch.

The script validates its inputs first and then:
1. Copies Kenji to a stable executable path (`C:\Program Files\Kenji\kenji.exe` by default), runs interactive `kenji setup` if necessary, and calls `kenji service install`.
2. Calls the official `ngrok service install --config <path>` only when an ngrok service does not already exist.
3. Configures both services for automatic start and three SCM recovery restarts at 60-second intervals.
4. Starts Kenji before ngrok and verifies both services reach `Running`.

You can rerun it after partial completion. Existing services are left intact: **the script does not replace a running Kenji executable or rewrite an already-registered ngrok service's configuration**. If ngrok was already installed as a service, `-NgrokConfig` may be omitted. If supplied, it is verified but does **not** update that service's existing settings. To change them, follow ngrok's service management documentation.

Stop any previous interactive `ngrok http` process using the same endpoint before enabling its service, otherwise duplicate endpoint sessions may conflict.

## Status and troubleshooting

- `Get-Service kenji,ngrok` — service status; the Kenji admin Settings page also reports native ngrok service state (not a public connection health check).
- `Get-WinEvent -LogName System` — Windows Service Control Manager failures; Kenji also logs to `C:\ProgramData\kenji\logs`.
- `sc.exe qfailure kenji` / `sc.exe qfailure ngrok` — inspect recovery actions.
- If ngrok cannot start, verify its config parses, is accessible to the service account, and uses the correct upstream port. A service in `Running` state does not guarantee the public endpoint is reachable.
- To remove services, run `ngrok service stop` / `ngrok service uninstall` and `Stop-Service kenji` / `kenji.exe service uninstall` using the relevant official CLIs.

## Security and scope

The existing Kenji service installer defaults to **LocalSystem** when a service account is not supplied. The bundle script refuses to create such a service unless `-AllowLocalSystem` is explicitly given. This makes remote shell/agent operations extremely privileged. Use a dedicated least-privilege service account and ngrok-side access policy for real deployments. Never expose Kenji to an untrusted network without additional access protection. Only deploy on machines and networks where the administrator has authorized remote access.

The bundled PowerShell script is an installer, **not a graphical setup wizard**. It does not distribute ngrok, provision domains, generate access policies, migrate existing service accounts, or implement an ngrok process supervisor. ngrok itself manages its separate service lifecycle.

Official ngrok service instructions: https://ngrok.com/download/windows
