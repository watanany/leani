#!/bin/sh
# vendor/yomiyasu/ の yomiyasu の lint を、GitHub の nanaism/yomiyasu の版に入れ替える。
# 引数はタグか commit (例: v2.5.0)。ファイルをすべて取得できたときだけ入れ替え、
# 取得した commit を vendor/yomiyasu/SOURCE に書く。
set -eu
ref=${1:?使い方: just yomiyasu-update <タグか commit>}
repo=nanaism/yomiyasu
dir=vendor/yomiyasu
commit=$(curl -fsSL -H 'Accept: application/vnd.github.sha' "https://api.github.com/repos/$repo/commits/$ref")
mkdir -p target
tmp=$(mktemp -d target/yomiyasu.XXXXXX)
trap 'rm -r "$tmp"' EXIT
for f in scripts/yomiyasu_lint.py scripts/markdown_visibility.py LICENSE UNICODE-LICENSE.txt; do
	curl -fsSL -o "$tmp/$(basename "$f")" "https://raw.githubusercontent.com/$repo/$commit/skills/yomiyasu/$f"
done
printf 'repo=https://github.com/%s\nref=%s\ncommit=%s\n' "$repo" "$ref" "$commit" >"$tmp/SOURCE"
mkdir -p "$dir"
mv -f "$tmp"/* "$dir"/
echo "yomiyasu の lint を $ref ($commit) に入れ替えた。just check で確かめる"
