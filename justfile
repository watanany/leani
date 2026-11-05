# テストと lint をすべて実行する。速く終わるものから順に実行する
check:
    cargo fmt --check
    uv run ruff format --check .
    uv run ruff check .
    uv run mypy
    cargo clippy --all-targets -- -D warnings
    cargo run -p spec -- --check
    cargo nextest run
    uv run pytest -n auto
