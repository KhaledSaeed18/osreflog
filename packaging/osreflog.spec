%global app_id io.github.khaledsaeed18.osreflog
%global selinuxtype targeted

Name:           osreflog
Version:        0.2.0
Release:        1%{?dist}
Summary:        System change history for Fedora: what changed, what changed it, and undo

# osreflog is GPL-3.0-or-later; the rest covers the bundled Rust crates and the SQLite they
# compile in (blessing), listed in LICENSE.dependencies. scripts/dist.sh checks this is current.
License:        GPL-3.0-or-later AND (0BSD OR Apache-2.0 OR MIT) AND (Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR CC0-1.0) AND (Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR MIT) AND (Apache-2.0 OR CC0-1.0 OR MIT-0) AND (Apache-2.0 OR MIT OR Zlib) AND (Apache-2.0 OR MIT) AND blessing AND MIT AND (MIT OR Unlicense) AND Unicode-3.0 AND Zlib
URL:            https://github.com/KhaledSaeed18/osreflog
# Built by scripts/dist.sh: the git tree plus vendored crates (offline build).
Source0:        %{url}/releases/download/v%{version}/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
# osreflogd's eBPF program (bins/osreflogd/bpf/) is compiled with clang.
BuildRequires:  clang
BuildRequires:  libbpf-devel
BuildRequires:  kernel-headers
BuildRequires:  systemd-rpm-macros
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1) >= 1.6
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
BuildRequires:  selinux-policy-devel
BuildRequires:  bzip2

# The dnf hook: a checkpoint before and after every transaction.
Requires:       libdnf5-plugin-actions
Requires:       dbus
Requires:       polkit
# Metal bisect probes: next_entry in grubenv, and an initramfs for a probe's own kernel.
Requires:       grub2-tools-minimal
Recommends:     dracut
# Container probes (systemd-nspawn) and VM probes.
Recommends:     systemd-container
Recommends:     qemu-kvm-core
Recommends:     virtiofsd
Suggests:       %{name}-app = %{version}-%{release}
# Its data under types of its own where SELinux is in use.
Requires:       (%{name}-selinux = %{version}-%{release} if selinux-policy-%{selinuxtype})

%description
osreflog keeps a history of a btrfs-rooted Fedora system: a read-only snapshot before and after
every package transaction and on a schedule, every change between them classified, and each
change tied to what made it (the package, the command, the user). It answers "what changed
since Tuesday?", undoes a change to /etc, /opt or /usr/local after showing exactly what it
will do, and bisects a regression across checkpoints by booting old states as containers,
VMs or real boots of the machine.

This package contains the daemon, the boot doctor, the per-user settings agent and the
osreflog command-line tool.

%package app
Summary:        GNOME app for the osreflog system history
Requires:       %{name} = %{version}-%{release}

%description app
A GNOME app to browse the osreflog history, see what changed since a checkpoint, undo a
change with a preview, and run a bisect step by step.

%package selinux
Summary:        SELinux types for osreflog's ledger and runtime files
BuildArch:      noarch
Requires:       selinux-policy-%{selinuxtype}
Requires(post): selinux-policy-%{selinuxtype}
%{?selinux_requires}

%description selinux
An SELinux module that labels osreflog's ledger, content blobs and attic in /var/lib/osreflog,
and its runtime directories, with types of their own, so confined services can't read the
history. The daemon itself keeps running unconfined.

%prep
%autosetup
# Build only from the crates vendored into the tarball.
mkdir -p .cargo
cat >> .cargo/config.toml <<'EOF'

[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF
# The distribution's toolchain, not rustup's.
rm -f rust-toolchain.toml

%build
export RUSTFLAGS="%{build_rustflags}"
cargo build --release --frozen --offline \
    -p osreflogd -p osreflog -p osreflog-user -p osreflog-app
make -C data/selinux -f %{_datadir}/selinux/devel/Makefile osreflog.pp
bzip2 -9 data/selinux/osreflog.pp

%install
install -Dpm0755 -t %{buildroot}%{_libexecdir}/%{name} target/release/osreflogd target/release/osreflog-user
install -Dpm0755 -t %{buildroot}%{_bindir} target/release/osreflog target/release/osreflog-app

install -Dpm0644 -t %{buildroot}%{_unitdir} data/systemd/osreflogd.service data/systemd/osreflog-boot-doctor.service
install -Dpm0644 -t %{buildroot}%{_userunitdir} data/systemd/osreflog-user.service
install -Dpm0644 -t %{buildroot}%{_datadir}/dbus-1/system.d data/dbus/*.conf
install -Dpm0644 -t %{buildroot}%{_datadir}/polkit-1/actions data/polkit/*.policy
install -Dpm0644 -t %{buildroot}%{_sysconfdir}/dnf/libdnf5-plugins/actions.d data/dnf/osreflog.actions
install -Dpm0644 -t %{buildroot}%{_sysconfdir}/%{name} data/etc/retention.conf
install -Dpm0644 -t %{buildroot}%{_datadir}/selinux/packages/%{selinuxtype} data/selinux/osreflog.pp.bz2
install -Dpm0644 -t %{buildroot}%{_datadir}/selinux/devel/include/distributed data/selinux/osreflog.if

install -Dpm0644 -t %{buildroot}%{_datadir}/applications data/app/%{app_id}.desktop
install -Dpm0644 -t %{buildroot}%{_metainfodir} data/app/%{app_id}.metainfo.xml
install -Dpm0644 -t %{buildroot}%{_datadir}/icons/hicolor/scalable/apps data/app/icons/%{app_id}.svg
install -Dpm0644 -t %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps data/app/icons/%{app_id}-symbolic.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstreamcli validate --no-net %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
export RUSTFLAGS="%{build_rustflags}"
cargo test --release --frozen --offline --workspace --exclude osreflog-fuzz

%post
%systemd_post osreflogd.service osreflog-boot-doctor.service
%systemd_user_post osreflog-user.service

%preun
%systemd_preun osreflogd.service osreflog-boot-doctor.service
%systemd_user_preun osreflog-user.service

# Upgrades restart the daemons at the end of the rpm transaction (systemd-update-helper), not
# halfway through it. Fresh installs leave the units to the presets: enabled by hand.
%postun
%systemd_postun_with_restart osreflogd.service
%systemd_postun osreflog-boot-doctor.service
%systemd_user_postun_with_restart osreflog-user.service

%pre selinux
%selinux_relabel_pre -s %{selinuxtype}

%post selinux
%selinux_modules_install -s %{selinuxtype} %{_datadir}/selinux/packages/%{selinuxtype}/osreflog.pp.bz2

%postun selinux
if [ $1 -eq 0 ]; then
    %selinux_modules_uninstall -s %{selinuxtype} osreflog
fi

%posttrans selinux
%selinux_relabel_post -s %{selinuxtype}

%files
%license LICENSE LICENSE.dependencies
%doc README.md
%{_bindir}/osreflog
%dir %{_libexecdir}/%{name}
%{_libexecdir}/%{name}/osreflogd
%{_libexecdir}/%{name}/osreflog-user
%{_unitdir}/osreflogd.service
%{_unitdir}/osreflog-boot-doctor.service
%{_userunitdir}/osreflog-user.service
%{_datadir}/dbus-1/system.d/io.github.khaledsaeed18.osreflog1.conf
%{_datadir}/polkit-1/actions/io.github.khaledsaeed18.osreflog.policy
%config(noreplace) %{_sysconfdir}/dnf/libdnf5-plugins/actions.d/osreflog.actions
%dir %{_sysconfdir}/%{name}
%config(noreplace) %{_sysconfdir}/%{name}/retention.conf

%files app
%license LICENSE
%{_bindir}/osreflog-app
%{_datadir}/applications/%{app_id}.desktop
%{_metainfodir}/%{app_id}.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

%files selinux
%license LICENSE
%{_datadir}/selinux/packages/%{selinuxtype}/osreflog.pp.bz2
%{_datadir}/selinux/devel/include/distributed/osreflog.if

%changelog
* Sat Oct 17 2026 Khaled Saeed <khaled18saeed@gmail.com> - 0.2.0-1
- The app: the open row marked, search, page headers, colored line diffs, animated loading,
  toasts, keyboard shortcuts; needs libadwaita 1.6
- Screenshots in the AppStream metainfo
- osreflog-selinux: the ledger and runtime files under types of their own
- Units: system call filters; the user agent sandboxed (exposure 8.0 to 1.0)

* Fri Oct 16 2026 Khaled Saeed <khaled18saeed@gmail.com> - 0.1.0-1
- First release: checkpoints around every dnf transaction and on a schedule, changes tied to
  what made them, undo with a preview, bisect in containers, VMs and real boots, and the app
