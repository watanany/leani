//! Tab 補完。問い合わせ回数は差し込み口で数える (時間で測ると結果が安定しないため)。
//!
//! Mathlib には定数が 47 万件あり、1 回の問い合わせに 1 秒かかる。名前空間ごとに
//! まとめて取得してキャッシュし、宣言を実行するたびにキャッシュを捨てないことが要点。

#![allow(
    non_snake_case,
    uncommon_codepoints,
    confusable_idents,
    mixed_script_confusables
)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use common::{Driver, cmd, die_once, sandbox};
use leani::engine::{Hooked, Stage};
use leani::repl::META_NAMES;
use leani_core::pure::meta_names;
use leani_core::queries::COMPLETE_QUERY;
use leani_read::complete::candidates;
use leani_read::config::config_envs;
use leani_read::places::Ctx;
use serde_json::json;
use story::story;

/// 定数を全件たどる問い合わせを数える差し込み口を入れる。重いのはこの問い合わせだけ。
fn count_chunk_queries(repl: &mut Driver) -> Arc<AtomicUsize> {
    let head = COMPLETE_QUERY.split("%s").next().unwrap_or("").to_string();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    repl.hook(move |stage| {
        if let Stage::Before(obj) = stage
            && cmd(obj).starts_with(&head)
        {
            seen.fetch_add(1, SeqCst);
        }
        Hooked::Pass
    });
    count
}

/// PATH とカレントディレクトリを、決まったファイルだけがある場所にした `Ctx`。
/// ディレクトリは `Ctx` と同じ寿命で持つ。
fn place() -> (Ctx, tempfile::TempDir) {
    let sb = sandbox();
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).expect("mkdir");
    for name in ["leanfoo", "leanbar"] {
        let file = bin.join(name);
        fs::write(&file, "").expect("write");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fs::write(bin.join("leannoexec"), "").expect("write");
    fs::write(dir.path().join("notes.lean"), "").expect("write");
    fs::write(dir.path().join("notes.md"), "").expect("write");
    let mut ctx = sb.ctx;
    ctx.cwd = dir.path().to_string_lossy().into_owned();
    ctx.path = bin.to_string_lossy().into_owned();
    (ctx, dir)
}

/// 候補の文字列。定数名の候補は常に Nat.succ を返す。
fn completions_in(ctx: &Ctx, text: &str, envs: &dyn Fn() -> Vec<String>) -> Vec<String> {
    let mut got: Vec<String> = candidates(
        ctx,
        text,
        &mut |_| vec!["Nat.succ".to_string()],
        envs,
        &META_NAMES,
    )
    .into_iter()
    .map(|c| c.text)
    .collect();
    got.sort();
    got
}

/// `place` の場所での候補。
fn completions(text: &str) -> Vec<String> {
    let (ctx, _dir) = place();
    completions_in(&ctx, text, &Vec::new)
}

/// 候補の (文字列, 置き換える範囲の先頭) と、問い合わせた prefix。
fn asked(text: &str, names: Vec<String>) -> (Vec<(String, usize)>, Vec<String>) {
    let (ctx, _dir) = place();
    let mut prefixes = Vec::new();
    let got = candidates(
        &ctx,
        text,
        &mut |prefix| {
            prefixes.push(prefix.to_string());
            names.clone()
        },
        &Vec::new,
        &META_NAMES,
    );
    (
        got.into_iter().map(|c| (c.text, c.start)).collect(),
        prefixes,
    )
}

mod 端末の_Tab_補完 {
    //! ja: 端末の Tab 補完
    //! en: Tab completion in the terminal
    use super::*;

    /// ja: #check Nat.suc には、カーソルの前の名前 Nat.suc だけを置き換える候補を返す
    /// en: Returns candidates that replace only the name before the cursor in #check Nat.suc
    #[story("D1")]
    #[test]
    fn Tab_で置き換える範囲はカーソルの前の名前だけ() {
        let text = "#check Nat.suc";
        let (got, prefixes) = asked(text, vec!["Nat.succ".to_string()]);
        assert_eq!(prefixes, ["Nat.suc"]);
        assert_eq!(
            got,
            [("Nat.succ".to_string(), text.len() - "Nat.suc".len())]
        );
    }
}

mod 略記の_Tab_補完 {
    //! ja: 略記の Tab 補完
    //! en: Tab completion of abbreviations
    use super::*;

    /// ja: 略記の途中で終わる入力には、その略記を記号に置き換える候補だけを返す
    /// en: Returns only the symbol replacing the abbreviation for input ending in a partial abbreviation
    #[story("C8")]
    #[test]
    fn 略記の途中なら記号を補完する() {
        let text = "theorem x : p \\alp";
        let (got, _) = asked(text, vec!["Nat.succ".to_string()]);
        assert_eq!(got, [("α".to_string(), text.len() - "\\alp".len())]);
    }

    /// ja: 略記の途中で終わる入力では、定数名を問い合わせない
    /// en: Does not query constant names for input ending in a partial abbreviation
    #[story("C8")]
    #[test]
    fn 略記の途中なら定数名を問い合わせない() {
        let (_, prefixes) = asked("\\to", Vec::new());
        assert!(prefixes.is_empty(), "{prefixes:?}");
    }

    /// ja: :t と :! の引数の末尾にある略記には、記号の候補を返す
    /// en: Returns a symbol candidate for an abbreviation at the end of the arguments of :t and :!
    #[story("C8")]
    #[test]
    fn コマンドの引数の中でも略記を補完する() {
        // space はどこでも略記を変換する。Tab も同じ範囲で変換する。
        assert!(completions(":t p \\to").contains(&"→".to_string()));
        assert!(completions(":! echo \\to").contains(&"→".to_string()));
    }
}

mod shell_コマンドの_Tab_補完 {
    //! ja: shell コマンドの Tab 補完
    //! en: Tab completion of shell commands
    use super::*;

    /// ja: 1 語目には、PATH にある実行できるコマンドを候補に返す
    /// en: Returns executable commands on PATH as candidates for the first word
    #[story("B5")]
    #[test]
    fn _1_語目は_PATH_にある実行できるコマンドを補完する() {
        assert_eq!(completions(":! leanf"), ["oo"]);
        assert_eq!(completions(":!lean"), ["bar", "foo"]);
    }

    /// ja: 2 語目からは、ファイルのパスを候補に返す
    /// en: Returns file paths as candidates from the second word on
    #[story("B5")]
    #[test]
    fn _2_語目からはファイルのパスを補完する() {
        assert_eq!(completions(":! cat notes."), ["lean", "md"]);
        assert_eq!(completions(":! cat bin/leanf"), ["oo"]);
    }

    /// ja: slash を含む 1 語目には、ファイルのパスを候補に返す
    /// en: Returns file paths as candidates for a first word containing a slash
    #[story("B5")]
    #[test]
    fn _1_語目でも_slash_を含めばパスを補完する() {
        assert_eq!(completions(":! ./notes.l"), ["ean"]);
    }

    /// ja: 定数名の形をした shell コマンドの引数には、定数名の候補を返さない
    /// en: Returns no constant names for a shell command argument that looks like a constant name
    #[story("B5")]
    #[test]
    fn shell_コマンドの中では定数名を補完しない() {
        assert!(completions(":! cat Nat.su").is_empty());
    }
}

mod コマンドの_Tab_補完 {
    //! ja: コマンドの Tab 補完
    //! en: Tab completion of commands
    use super::*;

    /// ja: : で始まる語には、HELP に載っているコマンド名を候補に返す
    /// en: Returns command names listed in HELP for a word starting with a colon
    #[story("H1")]
    #[test]
    fn コマンド名は_HELP_に載っているものを補完する() {
        assert_eq!(completions(":re"), ["reload", "reset", "restart"]);
        assert_eq!(completions(":"), *META_NAMES);
    }

    /// ja: HELP の文字列からは、英字のコマンド名だけを取り出す
    /// en: Extracts only alphabetic command names from the HELP text
    #[story("H1")]
    #[test]
    fn HELP_から_コマンド名だけを取り出す() {
        let help_text = "  :t, :type <expr>  型\n  :{ ... :}  囲む\n  :! <cmd>\n(:l で読む)";
        assert_eq!(meta_names(help_text), ["l", "t", "type"]);
    }

    /// ja: :l の後には、lean ファイルとディレクトリだけを候補に返す
    /// en: Returns only lean files and directories as candidates after :l
    #[story("B4")]
    #[test]
    fn colon_l_の後は_lean_ファイルとディレクトリだけを補完する() {
        assert_eq!(completions(":l notes."), ["lean"]);
        assert_eq!(completions(":load "), ["bin", "notes.lean"]);
    }

    /// ja: :save の後には、lean ファイルのパスだけを候補に返す
    /// en: Returns only lean file paths as candidates after :save
    #[story("B3")]
    #[test]
    fn colon_save_の後も_lean_ファイルのパスを補完する() {
        assert_eq!(completions(":save no"), ["tes.lean"]);
    }

    /// ja: :env の後には、環境の名前を候補に返す
    /// en: Returns environment names as candidates after :env
    #[story("G5")]
    #[test]
    fn colon_env_の後は設定ファイルに書いた環境の名前を補完する() {
        let (ctx, _dir) = place();
        let envs = || vec!["mathlib".to_string(), "plain".to_string()];
        assert_eq!(completions_in(&ctx, ":env ", &envs), ["mathlib", "plain"]);
        assert_eq!(completions_in(&ctx, ":env ma", &envs), ["mathlib"]);
    }

    /// ja: 設定ファイルを TOML として読めなければ、環境の名前を 1 つも返さない
    /// en: Returns no environment names if the config file is not valid TOML
    #[story("G5")]
    #[test]
    fn 設定ファイルを読めなければ環境の名前を補完しない() {
        let sb = sandbox();
        fs::write(&sb.ctx.config, "default = \"core\"\n[env\n").expect("write");
        assert!(config_envs(&sb.ctx).is_empty());
    }

    /// ja: :t の後には、定数名を候補に返す
    /// en: Returns constant names as candidates after :t
    #[story("D1")]
    #[test]
    fn colon_t_の後は定数名を補完する() {
        assert_eq!(completions(":t Nat.su"), ["Nat.succ"]);
    }
}

mod 名前空間ごとのキャッシュ {
    //! ja: 名前空間ごとのキャッシュ
    //! en: Cache per namespace
    use super::*;

    /// ja: 同じ名前空間の名前を続けて補完すると、定数を 1 回だけ問い合わせる
    /// en: Queries constants only once when names in the same namespace are completed in a row
    #[story("D1")]
    #[test]
    fn 同じ名前空間なら_1_回しか問い合わせない() {
        let mut repl = Driver::new();
        let asked = count_chunk_queries(&mut repl);
        assert!(
            repl.repl
                .complete_names("Nat.suc")
                .contains(&"Nat.succ".to_string())
        );
        assert_eq!(asked.load(SeqCst), 1);
        repl.repl.complete_names("Nat.succ_l");
        repl.repl.complete_names("Nat.ad");
        assert_eq!(asked.load(SeqCst), 1, "同じ単位の候補を取得し直している");
    }

    /// ja: 別の名前空間の名前を補完すると、定数を問い合わせ直す
    /// en: Queries constants again when a name in another namespace is completed
    #[story("D1")]
    #[test]
    fn 別の名前空間なら取得し直す() {
        let mut repl = Driver::new();
        let asked = count_chunk_queries(&mut repl);
        repl.repl.complete_names("Nat.suc");
        repl.repl.complete_names("List.ma");
        assert_eq!(asked.load(SeqCst), 2);
    }

    /// ja: 宣言を実行したあとに同じ名前を補完すると、定数を問い合わせ直さない
    /// en: Does not query constants again when the same name is completed after a declaration runs
    #[story("D1")]
    #[test]
    fn 宣言を実行してもキャッシュを捨てない() {
        let mut repl = Driver::new();
        repl.repl.complete_names("Nat.suc");
        repl.feed(&["def myOwnHelper := 1"]);
        // 構文の判定にも query を使うので、宣言を実行したあとから数える。
        // 環境が変わると open の一覧を問い合わせ直すが、定数を全件たどらないので
        // 数えない。
        let asked = count_chunk_queries(&mut repl);
        repl.repl.complete_names("Nat.suc");
        assert_eq!(asked.load(SeqCst), 0, "宣言のたびに取得し直している");
    }

    /// ja: 2 つの名前空間を open したあとに補完すると、定数を 1 回だけ問い合わせる
    /// en: Queries constants only once when a name is completed after two namespaces are opened
    #[story("D1", "D5")]
    #[test]
    fn open_した名前空間の名前も一度に取得する() {
        let mut repl = Driver::new();
        repl.feed(&["open Lean", "open Nat"]);
        let asked = count_chunk_queries(&mut repl);
        repl.repl.complete_names("Json.pa");
        assert_eq!(asked.load(SeqCst), 1);
    }
}

mod 候補 {
    //! ja: 候補
    //! en: Completion candidates
    use super::*;

    /// ja: :undo で宣言を取り消すと、その宣言の名前を候補から外す
    /// en: Removes the name of a declaration from candidates when :undo undoes the declaration
    #[story("D1", "F2")]
    #[test]
    fn colon_undo_で宣言を取り消すとその名前は候補に出ない() {
        // 宣言を実行したあとに問い合わせると、その宣言の名前もキャッシュに入る。
        let mut repl = Driver::new();
        repl.feed(&["def myUndone.helper := 1"]);
        let got = repl.repl.complete_names("myUndone.he");
        assert_eq!(got, ["myUndone.helper"]);
        repl.feed(&[":undo"]);
        assert!(repl.repl.complete_names("myUndone.he").is_empty());
    }

    /// ja: :undo のあとに同じ数の宣言を足すと、取り消した名前を外して足した名前を返す
    /// en: Returns the added name instead of the undone one when :undo is followed by a new declaration
    #[story("D1", "F2")]
    #[test]
    fn colon_undo_のあとに同じ数の宣言を足しても取り消した名前は候補に出ない() {
        // 宣言の数が同じなので、数だけを比べると前の名前を使い続ける。
        let mut repl = Driver::new();
        repl.feed(&["def myGone.helper := 1"]);
        assert_eq!(repl.repl.complete_names("myGone.he"), ["myGone.helper"]);
        repl.feed(&[":undo", "def myKept := 2"]);
        assert!(repl.repl.complete_names("myGone.he").is_empty());
        assert_eq!(repl.repl.complete_names("myKep"), ["myKept"]);
    }

    /// ja: :undo のあとに定数の問い合わせが失敗すると、取り消した名前を候補に返さない
    /// en: Does not return the undone name when the constant query fails after :undo
    #[story("D1", "F2")]
    #[test]
    fn 問い合わせ直しに失敗しても_colon_undo_で取り消した名前は候補に出ない() {
        // キャッシュを使えないときに問い合わせ直しに失敗しても、前の環境の名前を返さない。
        let mut repl = Driver::new();
        repl.feed(&["def myLost.helper := 1"]);
        assert_eq!(repl.repl.complete_names("myLost.he"), ["myLost.helper"]);
        repl.feed(&[":undo"]);
        let head = COMPLETE_QUERY.split("%s").next().unwrap_or("").to_string();
        repl.hook(move |stage| match stage {
            Stage::Before(obj) if cmd(obj).starts_with(&head) => Hooked::Reply(json!({})),
            _ => Hooked::Pass,
        });
        assert!(repl.repl.complete_names("myLost.he").is_empty());
    }

    /// ja: 宣言のあとに :reset を実行すると、その宣言の名前を候補に返さない
    /// en: Does not return the name of a declaration when :reset runs after the declaration
    #[story("D1", "F2")]
    #[test]
    fn colon_reset_で取り消した宣言は候補に出ない() {
        let mut repl = Driver::new();
        repl.feed(&["def myReset.helper := 1"]);
        let got = repl.repl.complete_names("myReset.he");
        assert_eq!(got, ["myReset.helper"]);
        repl.feed(&[":reset"]);
        assert!(repl.repl.complete_names("myReset.he").is_empty());
    }

    /// ja: 宣言を実行したあとに補完すると、その宣言の名前を返す
    /// en: Returns the name of a declaration when it is completed after the declaration runs
    #[story("D1")]
    #[test]
    fn 自分で実行した宣言も候補に表示される() {
        let mut repl = Driver::new();
        repl.feed(&["def myOwnHelper := 1"]);
        assert_eq!(repl.repl.complete_names("myOwnH"), ["myOwnHelper"]);
    }

    /// ja: 1 文字の名前を補完すると、エンジンに問い合わせずに空の一覧を返す
    /// en: Returns no candidates without querying the engine when a one-letter name is completed
    #[story("D1")]
    #[test]
    fn _1_文字では候補を表示しない() {
        // Mathlib だと `C` だけで 7.5 万件になる。leani は問い合わせもしない。
        let mut repl = Driver::new();
        let asked = Arc::new(AtomicUsize::new(0));
        let seen = asked.clone();
        repl.hook(move |stage| {
            if let Stage::Before(_) = stage {
                seen.fetch_add(1, SeqCst);
            }
            Hooked::Pass
        });
        assert!(repl.repl.complete_names("N").is_empty());
        assert_eq!(asked.load(SeqCst), 0);
    }
}

mod エンジンが異常終了したあとの_Tab_補完 {
    //! ja: エンジンが異常終了したあとの Tab 補完
    //! en: Tab completion after the engine crashes
    use super::*;

    /// ja: 補完するとエンジンを再起動せずに空の候補を返し、次の入力で再起動して宣言を残す
    /// en: Returns nothing without a restart on completion and restarts with declarations on the next input
    #[story("D1", "F1")]
    #[test]
    fn 補完ではエンジンを再起動せず次の入力で再起動する() {
        // Tab 補完は行編集の途中で呼ばれる。そこで再起動すると、再起動の表示が
        // 入力の行に混ざり、証明モードなどの状態も失われる。
        let mut repl = Driver::new();
        repl.feed(&["def survivor := 9"]);
        let generation = repl.repl.eng().generation();
        die_once(&mut repl, |_| true);

        let mut names = Vec::new();
        let out = repl.ask(|r| names = r.complete_names("Nat.suc"));

        assert!(!out.contains("再起動"), "{out}");
        assert_eq!(repl.repl.eng().generation(), generation, "補完で再起動した");
        assert!(names.is_empty(), "{names:?}");

        let out = repl.feed(&["survivor"]);
        assert!(out.contains("再起動する"), "{out}");
        assert!(out.contains('9'), "{out}");
    }
}

mod open_した名前空間 {
    //! ja: open した名前空間
    //! en: Opened namespaces
    use super::*;

    /// ja: 名前空間を open すると、その名前空間を省いた名前を候補に返す
    /// en: Returns names without the namespace prefix when the namespace is opened
    #[story("D5")]
    #[test]
    fn open_すると短い名前で補完される() {
        let mut repl = Driver::new();
        repl.feed(&["open Lean"]);
        assert!(
            repl.repl
                .complete_names("Json.pa")
                .contains(&"Json.parse".to_string())
        );
    }

    /// ja: 名前空間を open していなければ、名前空間を省いた名前を候補に返さない
    /// en: Does not return names without the namespace prefix if the namespace is not opened
    #[story("D5")]
    #[test]
    fn open_しなければ短い名前では補完されない() {
        let mut repl = Driver::new();
        assert!(repl.repl.complete_names("Json.pa").is_empty());
    }

    /// ja: open X in を実行したあとは、名前空間を省いた名前を候補に返さない
    /// en: Does not return names without the namespace prefix after open X in runs
    #[story("D5")]
    #[test]
    fn open_X_in_の効果は次の入力に残らない() {
        let mut repl = Driver::new();
        repl.feed(&["open Lean in #check 1"]);
        assert!(repl.repl.complete_names("Json.pa").is_empty());
    }

    /// ja: hiding を付けて open すると、隠した名前を外し、ほかの名前を返す
    /// en: Omits hidden names but returns the others when a namespace is opened with hiding
    #[story("D5")]
    #[test]
    fn hiding_した名前は補完されない() {
        let mut repl = Driver::new();
        repl.feed(&["open Lean hiding Json"]);
        let names = repl.repl.complete_names("Js");
        assert!(!names.contains(&"Json".to_string()), "{names:?}");
        assert!(names.contains(&"JsonNumber".to_string()), "{names:?}");
    }

    /// ja: 別の名前を隠して 2 回 open すると、どちらの名前も返す
    /// en: Returns both hidden names when a namespace is opened twice hiding a different name each time
    #[story("D5")]
    #[test]
    fn もう一度_open_して_hiding_を外した名前は補完される() {
        // 2 回目の open Lean は JsonNumber だけを隠すので、Json はソースに書ける。
        let mut repl = Driver::new();
        repl.feed(&["open Lean hiding Json", "open Lean hiding JsonNumber"]);
        let names = repl.repl.complete_names("Js");
        assert!(names.contains(&"Json".to_string()), "{names:?}");
        assert!(names.contains(&"JsonNumber".to_string()), "{names:?}");
    }

    /// ja: 名前を指定して open すると、指定した名前だけを名前空間を省いて返す
    /// en: Returns only the listed names without the namespace prefix when open lists names
    #[story("D5")]
    #[test]
    fn 名前を指定して_open_したものだけが補完される() {
        let mut repl = Driver::new();
        repl.feed(&["open Nat (succ_le_succ)"]);
        let names = repl.repl.complete_names("succ_le");
        assert!(names.contains(&"succ_le_succ".to_string()), "{names:?}");
        assert!(!names.contains(&"succ_le_of_lt".to_string()), "{names:?}");
    }

    /// ja: open すると、protected でない名前だけを名前空間を省いて返す
    /// en: Returns only non-protected names without the namespace prefix when the namespace is opened
    #[story("D5")]
    #[test]
    fn protected_な名前は最後の部分だけでは補完されない() {
        // Nat.add_comm は protected なので `open Nat` しても add_comm とは書けない。
        let mut repl = Driver::new();
        repl.feed(&["open Nat"]);
        assert!(
            !repl
                .repl
                .complete_names("add_com")
                .contains(&"add_comm".to_string())
        );
        assert!(
            repl.repl
                .complete_names("succ_le_s")
                .contains(&"succ_le_succ".to_string())
        );
    }

    /// ja: namespace を実行すると、その名前空間の名前を、名前空間を省いて返す
    /// en: Returns names of a namespace without its prefix when a namespace command has run
    #[story("D5")]
    #[test]
    fn namespace_の中ではその名前空間の名前も短い名前で補完される() {
        let mut repl = Driver::new();
        repl.feed(&["namespace Lean.Json"]);
        assert!(
            repl.repl
                .complete_names("pars")
                .contains(&"parse".to_string())
        );
    }
}
