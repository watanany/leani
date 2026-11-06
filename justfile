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

# main-rs から枝を切る。main-rs の上にいなければ、理由を出して止まる
start name:
    cargo run -q -p dev-checks -- can-start {{ quote(name) }}
    git switch -c {{ quote(name) }}

# just check を実行し、通れば今の枝を main-rs に fast-forward で merge して、枝を消す
finish:
    #!/bin/sh
    set -eu
    hooks() { cargo run -q -p dev-checks -- "$@"; }
    base=$(hooks base)
    branch=$(git branch --show-current)
    head=$(git rev-parse HEAD)
    old=$(git rev-parse "$base")
    hooks can-finish
    just check
    # just check の途中で commit が増えたり、ファイルが変わったりしていないことを確かめる
    hooks can-finish --head "$head"
    git switch "$base"
    git merge --ff-only "$head"
    # merge していない commit が枝に増えていれば、git は枝を消さずに止まる
    git branch -d "$branch"
    hooks after-finish "$old" "$head"

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
    cargo run -p docs-langs-check -- --check
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

# 今の枝と、開発の手順から外れていることの知らせを出す (crates/dev-checks)
status:
    cargo run -q -p dev-checks -- status

# main-rs から足したテストの文を出し、target/dev-checks/test-texts.txt にも書く (スキル leani-test-text)
test-texts:
    cargo run -q -p dev-checks -- test-texts

# AI の判断 (target/dev-checks/test-texts.json) が今の文と合っているかを確かめ、ng の文を出す (スキル leani-test-text)
test-texts-check:
    cargo run -q -p dev-checks -- test-texts-check

# main-rs から足した ja: の文を yomiyasu の lint にかける (スキル leani-test-text)
test-texts-lint:
    cargo run -q -p dev-checks -- test-texts-lint

# 日本語の文書を yomiyasu の lint にかけ、DEVELOPMENT の決まりと食い違う指摘を外して出す
[positional-arguments]
lint-ja +files:
    cargo run -q -p dev-checks -- lint-ja "$@"

# 片方の言語だけを変えた文書と、その差分を出す (スキル leani-docs-sync)
langs-diff:
    cargo run -q -p dev-checks -- langs-diff

# 日本語の文書に足した glossary.toml の語が、英語の文書にあるかを確かめる (スキル leani-docs-sync)
glossary-check:
    cargo run -q -p dev-checks -- glossary-check
