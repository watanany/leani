#!/bin/sh
# leani の開発の手順から外れたことを、AI に知らせる hook。中身は crates/claude-hooks にある。
# 知らせるだけで、AI を止め続けることはしない。hook が失敗しても、作業は続けられる。
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"
cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0
# ほかの cargo のコマンドとロックを取り合わないように、別の target ディレクトリでビルドする。
exec cargo run -q --offline --target-dir target/hooks-build -p claude-hooks -- "$@"
