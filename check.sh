#!/bin/sh
# Runs ic-plugin-check over every library in bin/: does each one load, what does
# it register, is its form drawable, and does it refuse a host it should not
# trust. Answers non-zero if any of them failed, so it belongs in CI.
#
# The checker lives in the closed tests checkout, not in plugin-api: it needs the
# application's document validator, which a plugin never links. Set IC_PLUGIN_CHECK
# to point at a built one.
set -e
cd "$(dirname "$0")"

case "$(uname -s)" in
    Darwin) EXT=dylib ;;
    MINGW*|MSYS*|CYGWIN*) EXT=dll ;;
    *) EXT=so ;;
esac

CHECKER=${IC_PLUGIN_CHECK:-}
if [ -z "$CHECKER" ]; then
    CHECK=../tests/plugin-check
    if [ ! -f "$CHECK/Cargo.toml" ]; then
        echo "no checker: set IC_PLUGIN_CHECK to a built ic-plugin-check." >&2
        exit 2
    fi
    # Built every time on purpose: cargo does nothing when it is already
    # current, and a checker left over from an older contract is worse than
    # no checker at all.
    (cd "$CHECK" && cargo build --release >/dev/null)
    CHECKER="$CHECK/bin/target/release/ic-plugin-check"
fi

libraries=$(ls bin/*."$EXT" 2>/dev/null || true)
if [ -z "$libraries" ]; then
    echo "nothing in bin/ to check — run ./build.sh first" >&2
    exit 2
fi

failed=0
for library in $libraries; do
    printf '%-32s' "$(basename "$library")"
    if summary=$("$CHECKER" "$library" --quiet 2>/dev/null); then
        echo "$summary" | tr -d '\n'
        echo
    else
        echo
        "$CHECKER" "$library" 2>/dev/null || true
        failed=$((failed + 1))
    fi
done

if [ "$failed" -ne 0 ]; then
    echo
    echo "$failed plugin(s) did not pass." >&2
    exit 1
fi
