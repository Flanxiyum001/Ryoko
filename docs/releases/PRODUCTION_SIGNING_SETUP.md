# Production desktop signing and updater activation (Gate A)

Status: setup instructions and proposed verification checks. No signing credentials have been provisioned by this change, and no production release is certified.

## Maintainer setup

Create the GitHub Actions environment `desktop-production` in repository Settings → Environments. Require independent human approval where your repository plan supports it, restrict deployment refs to the reviewed default branch and `desktop-v*` release tags, and prevent unauthorized tag creation using repository rules. Merely naming an environment in YAML does not configure protections.

Add the following secrets to that protected environment, not to source or PR jobs:

| Platform | Secrets | Source |
| --- | --- | --- |
| macOS | APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD, APPLE_SIGNING_IDENTITY | Developer ID Application certificate exported as a password-protected base64 P12 and its exact signing identity |
| macOS | APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID | Developer account, notarization app-specific password and Team ID |
| macOS | KEYCHAIN_PASSWORD | Separate randomly generated temporary keychain password |
| Windows | WINDOWS_CERTIFICATE, WINDOWS_CERTIFICATE_PASSWORD | Trusted code-signing certificate as a base64 PFX if exportable and supported by the issuer |

Modern Windows code-signing certificates may require hardware-backed keys or a managed signing service. If your issuer does not allow PFX export, adapt the signing workflow to that provider; do not extract private keys or weaken policy. Keep all credentials out of chats, screenshots and build logs.

Once the build inputs are reviewed, dispatch the production workflow from the approved ref and approve the environment deployment. It builds artifacts, not a public production release. Verify macOS code signature, stapled notarization and Gatekeeper; verify both Windows installer signatures, expected signer and timestamp. Record exact asset hashes, provenance and clean-machine installation evidence before authorizing any public distribution. The preview workflow remains distinct.

## Signed updater activation (preview channel only)

The existing [updater ADR](../decisions/017-in-app-updates.md) and implementation support an opt-in signed preview updater. On a trusted local machine with the locked desktop dependencies installed, generate a dedicated preview key:

```sh
cd apps/desktop
npm ci
npm run tauri -- signer generate -w "$HOME/ryoko-preview-updater.key"
```

Use a strong password and keep an encrypted offline backup. Set GitHub repository variable `RYOKO_UPDATER_PUBKEY` to the generated public key text and repository secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to the private key text and password. The preview build expects these exact names. Do not share the key or password with an assistant. This key signs updater artifacts, not OS installers. Keep a future production update channel and signing key separate.

Build and install two consecutive increasing preview versions on each supported OS. Check the fixed `desktop-updates/latest.json` manifest references the expected versioned assets; confirm the installed app downloads only on explicit consent, rejects modified payload/signature and wrong key, rejects downgrade attempts and survives interrupted download. Test unsaved-file handling and restart; record recovery steps and key-loss/rotation procedure. Do not enable a production updater until its own channel, versioning, signatures, protected publication and rollback behavior are reviewed and tested.

See [Phase 5 release trust](../PHASE_5_RELEASE_TRUST.md). The updater preview uses repository-level keys because it is automated on reviewed branch pushes; it must never receive a production updater or OS signing key.
