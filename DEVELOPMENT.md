# Development notes

Notes on what to keep in mind when changing leani locally.

leani is written in Rust (`crates/`). Only the Jupyter kernel runs on the Python version (`src/leani/`).
The Python version is kept only for the kernel, and only bugs in the kernel are fixed in it.

## Installation

To install from a cloned repository, run the following command in the repository.
The Rust toolchain is required.

```
cargo install --path crates/leani
```

After changing the code, run this command again. To try leani without installing it, use
`cargo run -p leani -- <引数>`.

Install the Jupyter kernel with the following commands. When it is installed this way,
changes to the Python code take effect at the next start.

```
uv tool install --editable '.[jupyter]' --force
leani-kernel install
```

## Tests and lint

`just check` runs all the tests and lints listed below. Install `just` with
`brew install just`.

The tests require the Rust toolchain, cargo-nextest, elan, and a built engine.
Build the engine with `cargo run -p leani -- --setup`.

```
cargo nextest run                          # run all tests
cargo nextest run -E 'binary(parsing)'     # run only the tests that use neither the terminal nor the engine
cargo nextest run history                  # filter tests by name
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run -p spec -- --check              # check that SPEC.ja.md and SPEC.en.md match the tests
cargo run -p docs-pair -- --check         # check that the English and Japanese documents are paired
```

The official way to run the tests is `cargo nextest run`. nextest runs each test in a separate
process. The tests also pass with `cargo test`, but then the tests run in parallel in the same process.

Run the tests and lints of the Jupyter kernel with the following commands.

```
uv sync
uv run pytest -n auto
uv run ruff format . && uv run ruff check .
uv run mypy
```

When you add or remove a test, or change the test sentences (the `ja:` and `en:` lines of doc comments and docstrings), the location of tests (the file and the `mod`), or the stories, run `cargo run -p spec` to regenerate `SPEC.ja.md` and `SPEC.en.md`.
The same applies when you change the kernel tests (`tests/test_kernel.py`).
If the content of `SPEC.ja.md` or `SPEC.en.md` does not match the tests, the X1 and X2 tests of `cargo nextest run` fail. `uv run pytest` does not fail.

## Workflow

- When adding a feature, write the story, the test, and the implementation, in that order. In this repository, the link between these three matters most. Add the story to `stories.toml`, and give the test the story ID with `#[story("A1")]` (`@story("A1")` for kernel tests). One story may have more than one test
- Put tests in `crates/leani/tests/`. `crates/spec` reads only this directory and `tests/test_*.py` to make `SPEC.ja.md` and `SPEC.en.md`, so unit tests inside a crate appear in neither. Write the test sentences one line each after `ja:` and `en:` in the doc comment of the test (the docstring for kernel tests). A `mod` is a describe, and the sentences of a `mod` go in `ja:` and `en:` of `//!` inside the `mod`. Make the test function name and the `mod` name from the `en:` sentence. Make it lowercase, replace each run of non-alphanumeric characters with `_`, and remove `_` at both ends (a kernel describe gets `describe_` in front). If the name is different, `crates/spec` reports an error. `crates/spec` also reports an error when the name made does not start with a letter, when it is a Rust or Python keyword, when a `mod` name is a crate name (`leani`, `spec`, etc.), and when a kernel test name starts with `describe`, so rewrite the `en:` sentence in those cases
- `crates/spec` reports an error for a test that appears in SPEC but may not run, and for a test that runs but does not appear in SPEC. Do not put `#[ignore]`, `#[cfg]`, or `#[cfg_attr]` on Rust tests. In kernel tests, do not use skip and xfail markers or `pytestmark`, do not write describes and tests under `if`, `for`, or `with`, and do not delete the name of a `def` with `del` or assign another value to it. Write `@story(...)` on one line, and write the number in double quotes, as in `"J1"`. Write the docstring right after the `def` line, start it with a line of only `"""`, and close it with a line of only `"""`. Put test files directly under `crates/leani/tests/` and `tests/`
- After changing code, read again the doc comment of the changed function, the comments in its callers and callees, and the documentation that describes that behavior. `crates/spec` detects differences between the tests and `SPEC.ja.md` and `SPEC.en.md`, but nothing detects differences between comments and documentation, or between `en:` sentences and `ja:` sentences. Of the descriptions of behavior, write the ones that can be checked with input and output in test sentences, not in comments
- Work on a branch, and when done, merge it into main and delete the branch. Do not create PRs
- Write commit messages in the Conventional Commits format, in Japanese

## Writing test sentences

Write test sentences so that they read in the order Given / When / Then (precondition, action, result).

- Tests: write 「(前提で) 〜すると 〜になる」 ("(given ...,) when ..., the result is ..."). Omit the Given when the describe sentence states it, and when there is no Given. Do not omit the Then. Example: 「証明モードで :help を実行すると、証明モードのヘルプを表示する」 ("shows the proof mode help when :help runs in proof mode")
- `mod` and describe: write a noun phrase (the Given, or the target of the test) or the form 「〜したとき」 ("when ..."). Examples: 「証明モードで送るタクティク」 ("tactics sent in proof mode") 「入力の判定の途中でエンジンが異常終了したとき」 ("when the engine crashes while judging input")

Depending on the kind of test, keep the following in mind.

- In a test that calls one function and checks the return value, write the input as the Given and the return value as the Then. The describe sentence states the action of calling the function, so do not make up a user action such as completion or running. Example: 「解釈できないバージョンには None を返す」 ("returns None for a version it cannot parse")
- In a `#[proptest]` test, write 「どの〜でも」 ("for any ...") and do not write it as if it were one example
- When you write 「〜しない」 ("does not ..."), also write the other results that the test checks. Example: 「:t を実行すると、型の代わりに取得できなかったと表示する」 ("says the type could not be obtained, instead of the type, when :t runs")
- Do not write 「〜できる」 ("can ..."). Write the result that the test checks
- Do not use 「も」 ("also") to refer to another test. In `SPEC.ja.md`, tests are listed by story, so the test it refers to is not nearby
- Do not write what the test does not check
- The only subjects that may be omitted are the subject of the result (leani, or the function named in the describe sentence) and the subject of user actions (入力する, 送る, 実行する, 押す: enter, send, run, press). This is an exception to "Do not omit subjects and objects" in "Writing documents". Do not omit the subject of other actions (セル, エンジン: cell, engine) or objects
- Do not write `\`. In Python docstrings, `\` is an escape. Write an abbreviation such as `\to` as 「to の略記」 ("the abbreviation of to")

An `en:` sentence is the English translation of the `ja:` sentence, and is written as follows. `crates/spec` does not check whether the `en:` sentence is a correct translation of the `ja:` sentence.

- Tests: start with the verb of the Then (third person singular), and continue with the When using `when` and the Given using `with`, `in`, `if`, `for`, and so on. Example: `Shows the proof mode help when :help runs in proof mode`
- `mod` and describe: write a noun phrase or the form `When ...` or `After ...`. Example: `Tactics sent in proof mode`
- Write only ASCII characters, and do not put `.` at the end. Do not write `\`
- Use the same English word for the same Japanese word. The word pairs are in `glossary.toml`, and if a ja sentence contains one of the words but the en sentence does not contain the English word, `crates/spec` reports an error

## Writing code

The crates are split by whether they have side effects.

| crate        | Contents                                                                  |
|--------------|---------------------------------------------------------------------------|
| `leani-core` | Pure functions and constants. The output depends only on the input        |
| `leani-read` | Reading. Reads environment variables and files, but changes nothing       |
| `leani`      | Side effects. Operates on processes, the terminal, files, and the network |
| `story`      | `#[story]` given to tests. Returns the function without doing anything    |
| `spec`       | Generates `SPEC.ja.md` and `SPEC.en.md`                                   |
| `docs-pair`  | Checks that the English `X.md` and the Japanese `X.ja.md` are paired      |

The list of modules of each crate is at the top of the `lib.rs` of that crate.
When adding code, follow these rules.

- As a rule, write code in a purely functional style. Keep Haskell in mind as the basic style. Return a new value rather than changing a value, and combine small functions
- Write branches with `match` where possible. If that is unnatural, write `if` as an expression
- In pure functions and reading functions, do not omit the `else` of an `if`. Make it clear from the shape of the code that every branch returns a value
- In functions with side effects, `else` may be omitted. As in Go, return early when a condition is not met
- Put judging and formatting in `leani-core`. Put only operations on processes, the terminal, and files in `leani`. This way, tests for judging and formatting can be written with only input and output
- Dependencies go only in the direction `leani-core` ← `leani-read` ← `leani`
- Only `main.rs` touches the state of the whole process (environment variables, the current directory, the SIGINT handler). `main.rs` reads environment variables only once at startup and makes `Ctx`, and other functions receive `Ctx` as an argument. The lib does not call `std::env::set_var` or `set_current_dir`. Pass these to child processes with `Command::env` and `current_dir`
- Only `Engine` and `Repl` hold the state of the session (the engine process, the declaration log, the input buffer, and so on). Make the fields of `Engine` private, and only the methods of `Engine` change them
- Do not print output with `print!`. Print it with `write` and `fail` of the `Output` (`self.out`) given to `Repl`. In tests, a type that only collects the output in a string receives it
- Create methods that let tests replace the communication with the engine only with the `testing` feature
- For lint, follow the common `clippy` settings. When they conflict with the rules of this repository, the rules of this repository take priority

The module rules of the Jupyter kernel (Python version) are in the docstring of `src/leani/__init__.py` and
the `ruff` settings in `pyproject.toml`.

## Writing documents

- Japanese documents use the plain form (だ/である)
- In Japanese documents, put a half-width space before and after alphanumeric characters. Do the same before and after code enclosed in `` ` ``
- Do not omit subjects and objects. Do not write in a way that leaves unclear what does what, as with 「入る」 ("goes in")
- Do not use metaphors or uncommon words. When one is pointed out, search for the same wording with `rg` and fix all of them together
- Write only what is needed, and do not list details with the same weight as important points. The README is the entry point, so do not write the history or paths of your local environment in it

## English and Japanese documents

The README, DEVELOPMENT, DECISIONS, and guide documents each have two files: the English `X.md` and the Japanese `X.ja.md`. Both are originals.

- You may add, change, or delete content starting from the document in either language. In the same commit, make the same change in the other document
- When translating, use the word pairs in `glossary.toml`
- When there is a reason to change only one of the documents (such as fixing a typo that exists in only one), write a `Docs-Pair-Skip: <reason>` line in the commit message
- `cargo run -p docs-pair -- --check` reports an error for a document that has no document in the other language, and for a commit that changes only one of the documents. It does not check uncommitted changes, so a commit that changes only one document is found when you run `just check` after the commit. Fix it before the merge with `git commit --amend` or `git rebase`
- `CLAUDE.md` is written only in Japanese. `SPEC.ja.md` and `SPEC.en.md` are generated by `crates/spec`

## Documentation

| File                                 | Contents                                                                                          |
|--------------------------------------|---------------------------------------------------------------------------------------------------|
| [README.md](README.md)               | Installation and the first start                                                                  |
| [guide/proof.md](guide/proof.md)     | Proof mode                                                                                        |
| [guide/config.md](guide/config.md)   | Configuration, environment variables, and file locations                                          |
| [guide/jupyter.md](guide/jupyter.md) | How to use leani as a Jupyter kernel                                                              |
| [SPEC.ja.md](SPEC.ja.md)             | List of what leani can do. `crates/spec` generates it from the tests                              |
| [SPEC.en.md](SPEC.en.md)             | English version of `SPEC.ja.md`. `crates/spec` generates it from the `en:` sentences of the tests |
| [DECISIONS.md](DECISIONS.md)         | Design decisions. The options that were not adopted, and the reasons                              |
| `stories.toml`                       | List of user stories                                                                              |
| `glossary.toml`                      | Pairs of Japanese and English words used in test sentences and documents                          |
| `crates/`                            | The code of leani. The reasons for the design are in doc comments and comments                    |
| `src/leani/`                         | Jupyter kernel (Python version)                                                                   |
