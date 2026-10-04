# Local transport bound

Source: published reis 0.7.1, MIT license, https://github.com/ids1024/reis.
src/wire/backend.rs bounds both undecoded incoming and unread outgoing
accumulation to 2 MiB and 64 stored file descriptors. Outgoing overflow
closes the connection and permanently stops further buffering.
src/wire/arg.rs propagates FD-clone errors so resource exhaustion closes
the connection instead of panicking.
The upstream 1 MiB individual-message bound remains. Exceeding either limit
fails the connection; Roost permanently revokes its RemoteDesktop grant.
Regression tests cover continuous undecoded bytes, unconsumed FDs and
unread outgoing responses.
This patch is required until equivalent upstream bounds are available.

This checked-in path dependency is kept under third-party/reis. Package builds
reserve root vendor/ for the registry closure produced by cargo vendor --locked;
the source archive must preserve third-party/reis separately from vendor.tar.gz.
