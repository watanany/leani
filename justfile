# 作業の流れと、コマンドの一覧を出す
default:
    @echo '作業の流れ (DEVELOPMENT.ja.md の「作業の流れ」)'
    @echo '  1. just start <枝>         枝を切る'
    @echo '  2. ストーリーとテストを書く'
    @echo '  3. テストが失敗するのを見る (cargo nextest run <名前>)'
    @echo '  4. 実装を書く'
    @echo '  5. just fix                整形し、SPEC を生成し直す'
    @echo '  6. just check              テストと lint をすべて実行する'
    @echo '  7. commit'
    @echo '  8. just finish             main-rs に merge して、枝を消す'
    @echo ''
    @just --list --unsorted

# 整形し、SPEC.ja.md と SPEC.en.md を生成し直す。.claude/ は書き換えない
fix:
    uv run ruff check --fix-only .
    uv run ruff format .
    cargo fmt
    bun fmt
    git ls-files -z --cached --others --exclude-standard -- '*.sh' ':(exclude).claude/' | xargs -0 shfmt -ln posix -w
    cargo run -q -p spec

# テストと lint をすべて実行する。速く終わるものから順に実行する
check:
    cargo fmt --check
    uv run ruff format --check .
    uv run ruff check .
    bun fmt --check
    just check-claude
    git ls-files -z --cached --others --exclude-standard -- '*.sh' | xargs -0 shellcheck
    git ls-files -z --cached --others --exclude-standard -- '*.sh' | xargs -0 shfmt -ln posix -d
    uv run mypy
    cargo clippy --all-targets -- -D warnings
    cargo run -p spec -- --check
    cargo run -p docs-pair -- --check
    cargo nextest run
    uv run pytest -n auto

# .claude/ の文書と設定の整形を確かめる。bunfig.toml で整形から外したファイルは、
# 名前を指定しても確かめずに通るので、名前を変えて stdin から整形し、元のファイルと比べる
[private]
check-claude:
    #!/bin/sh
    set -eu
    tmp=$(mktemp)
    trap 'rm -f "$tmp"' EXIT
    bad=0
    for f in .claude/skills/*/SKILL.md .claude/settings.json; do
        bun fmt --stdin-filepath="$(basename "$f")" < "$f" > "$tmp"
        if ! cmp -s "$tmp" "$f"; then
            echo "整形がずれている (Edit で直す): $f"
            bad=1
        fi
    done
    exit "$bad"

# 今の枝と、開発の手順から外れていることの知らせを出す (crates/claude-hooks)
status:
    cargo run -q -p claude-hooks -- status

# main-rs から足したテストの文を出す (スキル leani-test-text)
texts:
    cargo run -q -p claude-hooks -- texts
