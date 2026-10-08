# CI-only exact-source controlled fault fixture; never a production package.
%global debug_package %{nil}
%global __strip /bin/true
%global __objcopy /bin/true
%global _build_id_links none
%global __brp_strip %{nil}
%global __brp_strip_static_archive %{nil}
%global __brp_strip_comment_note %{nil}
%global __brp_ldconfig %{nil}
Name: roost-night-light-fixture
Version: 0.1
Release: 1
Summary: Isolated Roost Night Light VM final-pass fault fixture
License: GPL-3.0-or-later
Source0: roost-vm-night-light-compositor
Source1: source-commit.txt
Source2: version.txt
Source3: binary-sha256.txt
Requires: gnome-settings-daemon >= 51.0
Requires: gnome-settings-daemon < 52
Requires: colord-libs

%description
Separately feature-built controlled-fault executable for VM negative testing.
This package does not replace or qualify the production Roost compositor.

%prep
%build
%install
install -Dm755 %{SOURCE0} %{buildroot}/usr/libexec/roost-vm-night-light-compositor
install -Dm644 %{SOURCE1} %{buildroot}/usr/share/roost-night-light-fixture/source-commit.txt
install -Dm644 %{SOURCE2} %{buildroot}/usr/share/roost-night-light-fixture/version.txt
install -Dm644 %{SOURCE3} %{buildroot}/usr/share/roost-night-light-fixture/binary-sha256.txt

%files
/usr/libexec/roost-vm-night-light-compositor
/usr/share/roost-night-light-fixture/source-commit.txt
/usr/share/roost-night-light-fixture/version.txt
/usr/share/roost-night-light-fixture/binary-sha256.txt
