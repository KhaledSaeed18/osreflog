#!/bin/bash
# Release artifacts in target/dist/: the source tarball (the committed tree, the crates vendored
# for an offline build, LICENSE.dependencies) and the SRPM; with --rpm also the binary RPMs,
# built here with the toolchain on PATH (--nodeps: rustup's cargo, not Fedora's).
#
#   scripts/dist.sh [--rpm]
#   copr-cli build osreflog target/dist/rpmbuild/SRPMS/osreflog-*.src.rpm
#
# Needs a clean git tree, and checks that the spec's Version and License agree with the
# workspace before building anything.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"
SPEC=packaging/osreflog.spec
DIST=target/dist
RPM=false
[ "${1:-}" = --rpm ] && RPM=true

[ -z "$(git status --porcelain)" ] || { echo "the git tree isn't clean: commit first" >&2; exit 1; }

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
spec_version=$(sed -n 's/^Version: *//p' $SPEC)
[ "$version" = "$spec_version" ] || { echo "Cargo.toml says $version, the spec $spec_version" >&2; exit 1; }
name=osreflog-$version

# The shipped binaries and everything they link (build scripts' own dependencies run only at
# build time), per crate for LICENSE.dependencies, and as one SPDX expression for the spec.
tree=$(cargo tree --locked --color never -e normal --prefix none --format '{p}	{l}' \
    --target "$(rustc -vV | sed -n 's/^host: //p')" \
    -p osreflogd -p osreflog -p osreflog-user -p osreflog-app)
deps=$(python3 -I - "$tree" <<'EOF'
import re, sys
crates, terms = set(), set()
# C libraries a crate compiles in, whose own license covers only the bindings.
bundled = {"libsqlite3-sys": ("SQLite", "blessing")}
for line in sys.argv[1].splitlines():
    if not line.strip():
        continue
    package, _, license = line.partition("\t")
    package = package.replace(" (*)", "").strip()
    license = license.replace("(*)", "").strip()
    if not license:
        sys.exit(f"{package} declares no license")
    crates.add(f"{package}: {license}")
    for term in re.split(r"\s+AND\s+", license.replace("/", " OR ")):
        alts = sorted(a.strip(" ()") for a in re.split(r"\s+OR\s+", term))
        terms.add(alts[0] if len(alts) == 1 else "(" + " OR ".join(alts) + ")")
    name = package.split()[0]
    if name in bundled:
        what, spdx = bundled[name]
        terms.add(spdx)
        crates.add(f"{what} (bundled by {package}): {spdx}")
gpl = "GPL-3.0-or-later"
print(" AND ".join([gpl] + sorted(terms - {gpl}, key=lambda t: t.strip("(").lower())))
print("\n".join(sorted(crates)))
EOF
)
license=$(head -1 <<<"$deps")
spec_license=$(sed -n 's/^License: *//p' $SPEC)
if [ "$license" != "$spec_license" ]; then
    echo "the spec's License doesn't match the shipped crates; it should be:" >&2
    echo "License:        $license" >&2
    exit 1
fi

rm -rf "$DIST"
mkdir -p "$DIST/staging"
git archive --format=tar --prefix="$name/" HEAD | tar -x -C "$DIST/staging"
echo "vendoring crates…"
cargo vendor --locked --versioned-dirs "$DIST/staging/$name/vendor" >/dev/null
tail -n +2 <<<"$deps" >"$DIST/staging/$name/LICENSE.dependencies"
tar -czf "$DIST/$name.tar.gz" -C "$DIST/staging" "$name"
rm -rf "$DIST/staging"

mode=-bs
args=()
if $RPM; then
    mode=-ba
    args=(--nodeps)
fi
rpmbuild "$mode" "${args[@]}" --define "_topdir $REPO/$DIST/rpmbuild" --define "_sourcedir $REPO/$DIST" $SPEC
find "$DIST" -name '*.rpm' -printf '%P\n'
