#!/usr/bin/env python3
"""Differential check of osreflog's rpmvercmp against rpm's own (python3-rpm).

Input: lines of `a<TAB>b<TAB>result` from `osreflog-fuzz vercmp-pairs N`.
"""
import sys

import rpm


def main() -> int:
    n = bad = 0
    with open(sys.argv[1], encoding="utf-8") as f:
        for line in f:
            a, b, r = line.rstrip("\n").split("\t")
            n += 1
            want = rpm.labelCompare(("0", a, "1"), ("0", b, "1"))
            if want != int(r):
                bad += 1
                if bad <= 10:
                    print(f"MISMATCH {a!r} {b!r}: osreflog {r}, rpm {want}")
    print(f"rpm {rpm.__version__}: {n} pairs, {bad} mismatches")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
