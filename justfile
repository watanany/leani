# テストと lint をすべて実行する。速く終わるものから順に実行する
check:
    cargo fmt --check
    uv run ruff format --check .
    uv run ruff check .
    uv run mypy
    cargo clippy --all-targets -- -D warnings
    cargo run -p spec -- --check
    cargo run -p docs-pair -- --check
    cargo nextest run
    uv run pytest -n auto

# 今の枝と、開発の手順から外れていることの知らせを出す (crates/claude-hooks)
status:
    cargo run -q -p claude-hooks -- status

# main-rs から足したテストの文を出す (スキル leani-test-text)
texts:
    cargo run -q -p claude-hooks -- texts
