# Local transport bound

Source: published reis 0.7.1, MIT license, https://github.com/ids1024/reis.
Only src/wire/backend.rs differs from the published crate: pre-decoder
socket accumulation is limited to 2 MiB and 64 received file descriptors.
The upstream 1 MiB individual-message bound remains. Exceeding either limit
fails the connection; Roost permanently revokes its RemoteDesktop grant.
Regression tests cover continuous undecoded bytes and unconsumed FDs.
This patch is required until equivalent upstream bounds are available.
