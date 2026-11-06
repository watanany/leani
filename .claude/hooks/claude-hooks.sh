#!/bin/sh
# leani の開発の手順から外れたことを、AI に知らせる hook。中身は crates/dev-checks にある。
# 知らせるだけで、AI を止め続けることはしない。hook が失敗しても、作業は続けられる。
#
# hook は、人がインストールした dev-checks だけを動かし、作業ツリーのコードをビルドしない。
# hook は sandbox の外で許可を求めずに動くので、作業ツリーからビルドすると、書き換えたコードが
# 確認なしに動くためである (DECISIONS.ja.md の「不採用: hook で作業ツリーのコードをビルドして動かす」)。
# インストールしていなければ、何もせずに終わる。
#
#     cargo install --path crates/dev-checks
bin="${CARGO_HOME:-$HOME/.cargo}/bin/dev-checks"
[ -x "$bin" ] || exit 0
cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0
exec "$bin" "$@"
