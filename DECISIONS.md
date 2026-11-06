# Design decisions

This file lists the options that were considered for leani and not adopted, the reasons, and what was done instead.
The reasons for the current design are in the doc comments in `crates/` and the docstrings in `src/leani/`.

## Input

### Rejected: Judging the end of the input with regular expressions

Regular expressions that check whether the input starts with a declaration keyword, or whether the brackets are closed, could judge whether the input has ended.
However, if the input has syntax that was not expected, leani confirms the input too early, or reads the next line as part of the same input.
Regular expressions cannot judge notation that the user defined.

Instead, leani runs the Lean parser in the current environment for each line, and has it judge whether the input can be read as a command, a term, or a sequence of tactics.
The parser does not run the input, so it has no side effects.
Judging one line takes about 60ms.

### Rejected: Confirming the input as soon as the parser can read it

Another option is to confirm the input as soon as the parser can read it as complete syntax.
However, `structure P where`, `def f := 1`, and `induction n with` can each be read as complete syntax on their own, but the next indented line can continue them.

Instead, leani keeps reading the input while indented lines follow, and confirms it at a blank line or a line without indentation.
A line that ends with `where`, `with`, `do`, or `by` waits for a blank line even when it is complete.
If the user enters an indented line right after the input is confirmed, leani joins the previous input and that line and reads them again.

### Rejected: Confirming abbreviations at every key press

The Lean extension for VS Code confirms an abbreviation and replaces it with a symbol at every key press after `\`.
However, 293 abbreviations in the abbreviation table are the beginning of another abbreviation (`\a`, `\all`, and `\alpha` are all in the table).
The Lean extension for VS Code handles this by looking ahead: it confirms the abbreviation when a character is entered that cannot make it any longer.
If leani did the same, the processing would change state at every key press, and the terminal tests would also become heavier.

Instead, leani does not confirm an abbreviation until space or Tab is pressed.
At that point, there is only one abbreviation the user can have entered, so a single pure function does the conversion.

### Rejected: Converting abbreviations with only one of space or Tab

Converting with space only leaves a space after the symbol.
When a character follows a symbol directly, as in `(·.succ)` or `(\to)`, the user has to delete that space every time.
If only Tab converts and space does not, a common form such as `a \to b` needs two keys, Tab and space, for each symbol.

Instead, both keys convert. Space inserts a space after the conversion, and Tab does not.
Tab converts only when the text before the cursor is a `\name` in the table. Any other Tab does completion.
Lean names do not contain `\`, so the characters the user entered decide whether Tab converts or does completion.
lean.nvim also converts with Tab without inserting a space while an abbreviation is being entered.

### Rejected: Using readline for line editing

readline can also be used for line editing and history.
However, readline on macOS is actually libedit, a different implementation of the same API.
leani would have to handle all of the following itself: the width of color codes in the prompt, a multi-line input becoming several history entries, rewriting the history file, and Ctrl-C not taking effect right away.

Instead, leani uses reedline (the Python version used prompt_toolkit).
reedline counts the width of the prompt itself, and makes Ctrl-C take effect right away.
leani joins a multi-line input into one entry with its own history type and appends it.

### Rejected: Using rustyline for line editing

rustyline can also be used for line editing.
However, rustyline completes up to the common part of the candidates on the first Tab, and shows the list of candidates on the second Tab.
It cannot show a menu of candidates below the input on one Tab, as prompt_toolkit, which the Python version used, does.

Instead, leani uses reedline.
reedline keys can only be bound to a fixed set of editing operations, but opening a menu is one of them, so the abbreviation conversion with space and Tab can be built as a menu.
How it is built is in the doc comment of `crates/leani/src/term.rs`, and the spike that checked it is in `crates/leani/examples/spike.rs`.

### Rejected: Handling a multi-line input in one editing buffer

With reedline's `Validator`, several lines are handled in one editing buffer, and the user can go back to a line and fix it before the input is confirmed.
However, to judge whether the input has ended, leani queries the engine for each line.
With `Validator`, this query happens inside reedline's line editing.
The terminal stays in raw mode, so Ctrl-C pressed during that time does not become SIGINT, and does not take effect until the query ends.
The same happens with prompt_toolkit's `multiline=True` in the Python version.
Also, leani sometimes sends several times from one input, and sometimes joins a confirmed input with the previous input and reads them again.
This does not fit the form "send one buffer once".

Instead, leani confirms one line at a time.
To fix an earlier line, the user discards the input with Ctrl-C, recalls the whole input from history with Ctrl-P, and fixes it.

### Rejected: Keeping the result of `x ← e` in a variable

Like GHCi, leani could keep the result of an IO action in a variable with `x ← e`, so that the next input can use it.
However, leani sends expressions with `#eval`, so evaluating an expression does not change the environment.
Keeping the result in the environment would need an unsafe method or `initialize`, and the relation to the file that `:save` writes would also become complicated.

Instead, leani wraps an expression that contains `(← e)` in `do` and sends it.
Within one input, the result of `(← e)` can be used.

### Rejected: Exiting leani when a problem occurs

Another option is to exit leani when it receives an error or input that was not expected.
However, when leani exits, all the declarations run so far are lost.
This is the biggest loss for the user.

Instead, leani limits the lost input to one line in every case.
For example, when a command run with `:!` is stopped with Ctrl-C, leani shows `^C` and keeps running.
When piped input has bytes that cannot be read as UTF-8, leani replaces those bytes, passes the input to Lean, and has Lean report a syntax error.

### Rejected: Discarding the unfinished block at the end of input

Another option is to discard the unfinished block and exit when piped input ends, or when Ctrl-D is pressed in the terminal.
However, if a file does not end with a blank line, the last block is not run.
In the terminal too, pressing Ctrl-D while entering the rest of a block loses that block.

Instead, leani confirms and runs the unfinished block, and then waits for input.
In the terminal, pressing Ctrl-D again exits.
A block that starts with `:{` and is not closed with `:}` has no known end, so leani does not run it and reports it.

### Rejected: Restoring the default action for SIGPIPE

Rust programs ignore SIGPIPE, so when the output is closed first, as in `leani | head`, a write fails and the program panics.
If SIGPIPE is restored to its default action, leani exits without printing anything, like many commands.
However, SIGPIPE also happens on writes to the engine.
If leani writes to the engine after the engine crashed, leani also exits, and the declarations so far are lost.

Instead, leani remembers that it can no longer write to the output, and exits with exit code 0 before it reads the next input.

### Rejected: Discarding the completion cache on `:undo` and `:reset`

To keep the names of undone declarations out of the Tab candidates, leani could discard the cache on `:undo` and `:reset`.
However, other operations also revert the environment (discarding an unfinished block, rewriting in proof mode, restart, and so on).
If a discard step is added for each operation, the same mistake happens in the next operation that is added.
This is the same reason why "Fixing the handling of a missing environment in each function that sends to the repl" was not adopted.

Instead, leani records, for each cache entry, the environment at the time the entry was fetched.
leani uses the cache only when that environment is a base of the current environment.

## Engine

### Rejected: Having the user clone and build the engine

Another option is to have the user clone and build leanprover-community/repl, separately from installing leani.
However, for the user, this only adds a second installation step.

Instead, leani prepares the engine in `~/.local/state/leani/engine/<バージョン>`.
The first time, cloning and building take time.

### Rejected: Keeping the engine in one place

Another option is to keep the engine in one directory and use it for all Lake projects.
However, whether the engine can read `.olean` files depends on the Lean version that was used to build the engine.
The engine would be built again every time the user moves between Lake projects with different Lean versions.

Instead, leani uses a separate directory for each Lean version.
From the second time on, the engine does not need to be built again.

### Rejected: Saving the session with pickle

Saving the session with the repl's `pickleTo` and `unpickleEnvFrom` looks like it would make the next start faster.
However, a pickle holds only the difference from the imports, so unpickling does not change the time to load `.olean` files.
In measurements, import took 1.3 seconds and unpickle took 1.2 seconds, and with Mathlib both took about 5.3 seconds.
Also, running `#eval` in an unpickled environment makes the Lean compiler PANIC.

Instead, leani writes the session to a Lean file with `:save`.
The written file can be edited, and can be run with `lean`.

### Rejected: Judging whether the import succeeded from the repl's response

It looks as if the repl's response can tell whether the import at startup succeeded.
However, when an import cannot be resolved, the repl discards the whole header without an error or any stderr output, and returns a new environment.
`import Lean` is discarded too, so leani starts in an environment where neither input judging nor completion works.
A typo such as `leani -i Leen.Meta` leads to this state.

Instead, leani runs `#check @Lean.Parser.runParserCategory` in the same request as the header, and judges by whether it succeeds.
At startup, this method does not add requests.
When a file is loaded with `:l`, leani runs the same check after loading it.

## Settings and switching environments

### Rejected: Leaving type mistakes in the settings to the code that uses the values

Another option is to not check the types of the values in the settings file, and to notice mistakes where the values are used.
However, TOML accepts anything, so `imports = "Mathlib"` is not an error either.
When the Python version took this value as it was, it tried to import each character of the string.
Even if an error is shown where the value is used, the user cannot tell which part of the settings is wrong.

Instead, leani checks the types of the settings before it starts, and shows the location of the wrong key in the error.

### Rejected: Stopping the current engine before `:env` finishes preparing

When `:env <name>` runs, leani could first stop the current engine, and then prepare the target (decide the Lean version, prepare the engine, run `lake env`).
However, if the preparation fails, the user loses the whole session.

Instead, leani finishes the preparation before it stops the current engine. If the preparation fails, leani does not stop the current engine.
After the preparation, leani stops the current engine, and then starts the engine for the target.
Even so, whether the import succeeds is not known until the engine starts.
If the start fails, leani starts the engine again in the original environment and replays the declarations entered so far.
If the start in the original environment also fails, the environment is lost (leani in the terminal exits).

### Rejected: Fixing the handling of a missing environment in each function that sends to the repl

When the engine fails to start, leani has no environment to run declarations in.
If a type query or a declaration is sent to the repl in this state, the repl does not return an error, and answers by creating a new environment that does not even have `import Lean`.
The returned type is wrong, and a declaration disappears after the repl answers that it succeeded.
Each function that sends to the repl could be fixed one by one, but 3 reviews found 4 mistakes of this kind, and 2 of them were in code added in the previous review.
If each function is checked one by one, the same mistake happens in the next code that is added.

Instead, leani checks in one place: the function that sends commands to the repl.
If there is no environment, this function returns `EngineFault::NoEnvironment`, and the caller decides what to do.
A caller that has not decided stops with a one-line "internal error", which makes the cause easier to find than a wrong type or a lost declaration.

## Theorem search

### Rejected: Using Lean's `#loogle` for theorem search

With `import LeanSearchClient` in the engine, `#loogle` can be used as a Lean command, so leani does not need any code for it.
However, the search runs inside the exchange with the engine, so when a search is stopped with Ctrl-C, leani has to create the engine again and replay the declarations.
The results come back as Lean messages, so leani cannot cut the results to the screen width or add module names.
Also, LeanSearchClient is a package that is added as a dependency, so search does not work in environments that do not use Mathlib.

Instead, leani sends HTTP requests to loogle itself.
The search runs separately from the engine, so searching does not change the environment.

## Jupyter

### Rejected: Using lean4_jupyter for the Jupyter kernel

There is already a Jupyter kernel for Lean 4: lean4_jupyter (https://github.com/utensil/lean4_jupyter).
When it was tried with Lean v4.34.1 in September 2026, cells ran, but the following features were missing.

- Completion (no candidates are returned)
- Recovery after the engine crashes (all later cells fail with errors)
- Replacing `sorry` when a proof is complete

leani already has all of these.
Instead, a kernel was made that uses leani's `Repl` (the Python version) as it is, and passes each line of a cell to `Repl` in the same order as when the lines are entered in the terminal.

### Rejected: Moving the Jupyter kernel to Rust together with the terminal REPL

When leani was moved to Rust, the Jupyter kernel could have been moved at the same time.
However, building a kernel in Rust means choosing and trying a crate that handles the message exchange with Jupyter (ZeroMQ), which is separate work from porting the terminal REPL.
If both were moved at the same time, the terminal REPL could not be switched until the port was finished.

Instead, the kernel stays in the Python version, and starts with the `leani-kernel` command.
The Python version is kept only for the kernel, and only kernel bugs are fixed in it.
Features added to the terminal REPL are not available in the kernel until the kernel is moved to Rust.

## Development

### Rejected: Writing the list of what leani can do in a document separate from the code

Another option is to maintain the list of what leani can do as a document separate from the code.
However, a document that cannot be derived mechanically from the code will sooner or later fall behind changes to the code.

Instead, the `ja:` and `en:` lines in the doc comments of the tests (docstrings in the Python version) become the spec sentences, and `SPEC.ja.md` and `SPEC.en.md` are generated from them.
There is no place to add text by hand.
If someone forgets to generate them, the tests that call `spec::check` (X1 and X2 in parsing.rs) fail.

This mechanism guarantees only that each spec sentence is right next to its test, and that changing a sentence also changes `SPEC.ja.md` and `SPEC.en.md`.
It does not guarantee that a sentence matches what the test does, or that an `en:` sentence matches its `ja:` sentence.
That can only be checked by reading the tests and the sentences.

### Rejected: Matching user stories and tests one to one

Another option is to make one `describe_` for each user story.
However, that would make the division of the tests follow the division of the stories.
The tests should be divided so that each test fails for only one reason, and is placed in the fastest layer possible.

Instead, each test names its stories with `#[story(...)]`, and `crates/spec` matches them.
Stories are a table for counting whether there are enough tests, so stories and tests can be many-to-many.

### Rejected: A macro that takes the test sentence as a string

In the Rust version, a proc-macro was prototyped that takes the test sentence as a string, as in `it!("単独の式は評価されて値が表示される", story = ["A1"], { ... })`.
However, the prototype showed the following.
Tests written with `it!` do not appear in the rust-analyzer outline.
rustfmt formats `it!(...)` as a function call, so a long sentence splits the call into several lines.
The name of the `mod` that serves as the describe stays an identifier, so there would be two ways of writing sentences.
The Python version cannot use macros, so Rust and Python would write sentences in different ways.

Instead, sentences are written on the `ja:` and `en:` lines of the doc comment (next section).
Function names do not get a prefix such as `it_`.
In Rust, `#[test]` and `#[proptest]` mark a test. In the Python version, `@story` inside a describe marks a test.
pytest-describe collects every function inside a describe whose name does not start with `_` as a test, so the Python version does not need a prefix either.
The `describe_` on the describe side is added because pytest-describe needs it to find describes.

### Rejected: Writing the test sentence as a Japanese function name

Another option is to make the test function name a Japanese sentence, and have `crates/spec` turn the function name back into the spec sentence.
This was the earlier form.
However, function names cannot contain symbols, so rules are needed to turn a function name back into a sentence, such as writing `:` as `colon_`.
Also, a test can hold only one sentence, so there is no place for an English sentence.
After it was decided that people who do not read Japanese should also be able to read the list (X2), this form was dropped.

Instead, sentences are written on the `ja:` and `en:` lines of the test's doc comment (docstring in the Python version), and the function name is made mechanically from the `en:` sentence.
There is no need to think of a separate name, so for each test, the writer only has to think of two sentences, one in Japanese and one in English.
In exchange, function names are long. rustfmt does not wrap names, so some `fn` lines are longer than 100 columns.
`crates/spec` does not check whether a sentence has the Given / When / Then form, or whether an `en:` sentence is a correct translation of the `ja:` sentence.
The person who writes the test checks them by following "Writing test sentences" in DEVELOPMENT.md.

### Rejected: Keeping test sentences in a file separate from the tests

Another option is to keep test sentences in a separate file such as `stories.toml`, and link them to the tests by ID.
This makes it easier to write the sentences in several languages.
However, that is the same as writing the list of what leani can do in a document separate from the code.
When the sentences are away from the tests, nothing fails if someone fixes a test and forgets to fix its sentence.

Instead, sentences are written on the `ja:` and `en:` lines of the test's doc comment.
leani also does not adopt the form that keeps only the `ja:` sentence next to the test and puts the translations in a separate file.
The translations would need a key to link them to the tests, and the key would have to be fixed every time a test is renamed.
When a translation is away from its test, it is easy to forget to fix the translation after fixing the `ja:` sentence.

If a third language is added, `ja:` and `en:` stay next to the tests, and the sentences in the third and later languages go in a file for each language.
In that case, each translation holds a hash of the sentence it was made from. When the original sentence changes, `crates/spec` can detect that the translation is out of date.
There are two languages now, so this mechanism is not built.

### Rejected: Leaving every step of the development skills to AI

For the development skills in `.claude/skills/` (review, checking test sentences, applying translations, and so on), another option is to leave every step, from collecting the targets to summarizing the results, to AI.
However, each time AI runs, the range it collects and the points it checks change.
Running the same skill twice on the same change does not always give the same result.

Instead, a skill is split into three stages.
The stage that collects the targets and the stage that checks and summarizes the results are scripts in the repository, run from `just`, with tests.
AI handles only the stage that makes judgments about the collected targets, and the form of its answer is fixed in advance with a JSON schema.
The input and output of the scripts are kept in files, so they can be checked again later with the same input.

For example, in the skill `leani-test-text`, `just test-texts` collects the test sentences and writes them to `target/dev-checks/test-texts.txt`.
AI writes its judgment of each sentence to `target/dev-checks/test-texts.json`, and `just test-texts-check` checks for missing and duplicate judgments, old sentences, and errors in the form.

`just test-texts-check` does not write the sentences that AI fixed back to the source.
If it did, errors in the judgment of AI would go into the source without a person reading them.
Fixing an `en:` sentence also means changing the name of the test function, and changing the name needs reading the code around it.
AI fixes the sentences with Edit, and starts again from `just test-texts`.

The yomiyasu lint (`just lint-ja`) is not part of `just check`.
The lint is in a plugin outside the repository, and its rules and output change with its version.
If it were part of `just check`, the same commit could give different results depending on the version of the installed plugin.

### Rejected: Forcing the development workflow with hooks

Claude Code hooks can also refuse a change before AI makes it to a file.
One option is to refuse the change when AI tries to change code without changing tests, or tries to change files on `main-rs`.
However, there are cases where leaving the workflow is fine.
A refactoring that does not change behavior does not change tests, and a typo that exists in only one language's document is fixed in only that document.
If the change is refused, AI tries to make the same change in another way that is not refused, and the reason for leaving the workflow is not recorded.

Instead, the hooks (`crates/dev-checks`) only notify AI.
When AI changes a file, the hook gives AI the notices related to that file.
When AI is about to finish, if there are notices, the hook stops AI once and has it fix them or write the reason for not fixing them in its reply to the user.
The same notice is not repeated within one session, so after AI decides to continue with a reason, the hook does not stop AI.

### Rejected: Building and running the code in the working tree from the hooks

If the hooks ran `cargo run -p dev-checks`, a change to `crates/dev-checks` would take effect in the hooks right away.
However, the hooks run outside the sandbox, without asking for permission.
If the code of `crates/dev-checks` or of a crate it depends on, or a `build.rs`, is rewritten, the rewritten code runs at the next hook without any confirmation.
The same applies when AI rewrites it, so code could run without going through the permission check for Bash.

Instead, the hooks run only the version that a person installed with `cargo install --path crates/dev-checks`.
If it is not installed, the hooks do nothing.
After `crates/dev-checks` changes, the hooks keep running the old version until it is installed again.
For the same reason, `dev-checks` does not let git run commands written in `.git/config`.
Many keys make git run a command, such as fsmonitor, external diff, textconv and filter, so a list of keys to block leaves gaps.
Instead, `dev-checks` lists the keys known not to run commands, and does not run git if the repository config (local and worktree) has any other key.
In that case the hooks say nothing, and `just status` shows the reason.
The config of submodules is not checked, so `git diff` does not run git inside submodules.
The user's global and system config is not checked, because the user wrote it.

### Rejected: Putting the code in one crate

Another option is to put the code in one crate, and divide the modules by whether they have side effects.
However, when references were counted in a plan that put the Python code in one file, there were 4 kinds of places where a section without side effects referred to a section with side effects.
Inside one crate, any module can refer to other modules through `crate::`, so a reference in the wrong direction still compiles, and the only way to notice it is to read the code.

Instead, the crates are divided by whether they have side effects (`leani-core` ← `leani-read` ← `leani`).
The direction of references becomes the direction of crate dependencies, so a reference in the wrong direction is a compile error.
Crate dependencies cannot prevent side effects through std functions, so `clippy.toml` in `leani-core` and `leani-read` forbids them.
The Python version divides modules by whether they have side effects, and checks the direction of imports with `ruff`.

### Rejected: Checking the direction of dependencies with tests

This repository writes its rules as tests, so the direction of dependencies and the forbidden functions could also be checked by tests that read the crate sources with syn.
However, that means maintaining our own code that walks the syntax tree.
Without knowing whether it catches every way of writing, such as aliases in `use` or calls inside macros, the rules would be treated as "kept because there is a test".

Instead, leani uses crate dependencies and clippy's `disallowed-methods` and `disallowed-types`.
No new command is added, and `cargo clippy` fails and shows the line that breaks a rule.
In exchange, these rules do not appear in `SPEC.ja.md` and `SPEC.en.md`, so they are written in the doc comment of each crate's `lib.rs`.

### Rejected: Validating repl responses with structs

If repl responses are read into structs with serde, the compiler can find mistakes in key names.
However, when the repl version changes and a type changes or a key disappears, the whole response becomes an error because of a key that leani does not use.
leani uses only part of each response, so it is better if changes in keys that leani does not use can be ignored.

Instead, leani receives a response as a `serde_json::Value`, and takes out only the keys it uses, at the place that reads them.
When a key is missing or has a different type, the place that reads it replaces it with a default value.
A mistake in a key name is not found at that place. This is known and accepted.
The Python version does not use pydantic for the same reason, and uses `TypedDict(total=False)`.

### Rejected: Using property tests for engine and terminal tests too

Another option is to use property tests not only for pure functions but also for engine and terminal tests.
However, each engine test includes exchanges with the repl, so generating 100 examples makes one test take several minutes.
Also, a counterexample at the engine layer only tells that "this sequence of operations went wrong", and the cause is hard to read from it.

Instead, property tests (proptest) are used only for tests of pure functions.
At the engine layer, leani uses `INVARIANTS` in `crates/leani/tests/common/mod.rs` (invariants checked after each line).
Invariants fail closer to the cause.

### Rejected: Listing the methods that keep each invariant in doc comments and INVARIANTS

Another option is to write, for each item in `INVARIANTS`, the names of the methods that keep that condition in doc comments and comments.
However, when a method is renamed, or a method that changes state is added, the list stops matching the implementation.
Tests cannot check whether the list is correct.

Instead, the fields of `Engine` are private, so that only methods of `Engine` can change them.
The compiler enforces this rule (in the Python version it was checked with `rg`).
How `held` and `proof` in `Repl` are handled will be decided inside the proof mode module, when proof mode is moved to a separate module.

### Rejected: Writing state transitions inside the procedures that operate the engine

The steps for switching environments with `:env` used to be written inside `Repl::cmd_env`, together with starting and stopping the engine and the output.
However, that left no place where one could read, as a list, what to do when a given result comes in a given state.
When the test sentences were rewritten, the writer missed that leani starts the engine again in the original environment when the target fails to start, and wrote the wrong sentence 「元の環境のエンジンは動いたままにする」 ("keeps the engine of the original environment running").

Instead, the transition after the start result is received is a pure function in `leani-core` (`switch::on_boot`), and `Repl::cmd_env` only does the "next step" that this function returns.
In the `match` in `on_boot`, one line shows that when the target fails to start, leani starts in the original environment (`GoBack`).
Tests check all combinations of start results (9 cases) without starting the engine. There are few combinations, so the test lists all of them instead of using a property test.
The steps that stop the current engine, and the first start of the target, remain in `cmd_env`. This is written in the doc of the `switch` module.

Only `:env` has this form now.
Whether `:restart` and recovery from engine crashes get the same form will be decided after seeing the effect on `:env`.

### Rejected: Staying with the Python version and keeping the rules with lint

leani's rules (pure modules do not import modules with side effects, only methods of `Engine` change the attributes of `Engine`, and so on) were checked with `ruff` and `rg` in the Python version.
However, some ways of writing, such as `import leani`, cannot be detected by `ruff`, and the `rg` check checks nothing if someone forgets to run it.

Instead, leani was moved to Rust.
The compiler and lint keep the rules through the direction of crate dependencies, private fields, and the functions and types forbidden in clippy.
Performance and ease of distribution were not the main reasons for the move.

### Rejected: Changing the state of the whole process in each test

The tests of the Python version changed environment variables, PATH, and the current directory with monkeypatch.
pytest-xdist runs tests in separate processes, so these changes did not affect other tests.
However, `cargo test` runs the tests in one test binary in parallel, on threads of the same process.
Changing environment variables or the current directory changes the results of other tests running at the same time.

Instead, `main.rs` reads environment variables once at startup and makes a `Ctx`, and other functions receive `Ctx` as an argument.
The lib does not change environment variables or the current directory, and passes them to child processes with `Command::env` and `current_dir`.
Each test makes its own `Ctx`.
The official way to run the tests is now `cargo nextest run`, which runs each test in a separate process.

## Documents

### Rejected: Making the Japanese documents the originals and the English documents translations

Another option is to make Japanese the original for the README, DEVELOPMENT, DECISIONS, and guide documents, and translate them into English.
However, then people who read English cannot fix the documents.
This is because a fix made only to an English document is lost the next time the Japanese is translated again.

Instead, both the English `X.md` and the Japanese `X.ja.md` are treated as originals.
Content may be added, changed, or removed from either language, and the same commit makes the same change in the other document.
If a commit changes only one of the two documents, `just check` fails (`crates/docs-langs-check`).
When there is a reason to change only one document, such as fixing a typo that exists in only one of them, write a `Docs-Langs-Skip:` line in the commit message.
Only commits are checked, not uncommitted changes. This is so that `just check` can run while one document is being edited.
The English document is `X.md` because GitHub shows README.md on the first page of the repository.
The check only confirms that the two documents changed in the same commit, so whether the translation matches can only be checked by reading it.

### Rejected: Generating the Japanese and English documents from one source

Another option is to write one file with `ja:` and `en:` side by side for each paragraph, and generate `X.md` and `X.ja.md` from it.
This is the same mechanism that makes `SPEC.ja.md` and `SPEC.en.md` from the test sentences, and it can report an error when a paragraph in one language is missing.
However, even headings and tables would have to be split line by line into `ja:` and `en:`, so the file could not be written as normal Markdown.
Editor previews would not work either.
If someone fixes a generated `X.md` directly, the fix is lost the next time it is generated.

Instead, leani keeps a normal Markdown document for each language (previous section).
