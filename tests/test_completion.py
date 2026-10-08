"""Tab 補完。問い合わせ回数は mocker で数える (時間で測ると結果が安定しないため)。

Mathlib には定数が 47 万件あり、1 回の問い合わせに 1 秒かかる。名前空間ごとに
まとめて取得してキャッシュし、宣言を実行するたびにキャッシュを捨てないことが要点。
"""

import pytest
from conftest import story
from prompt_toolkit.completion import CompleteEvent
from prompt_toolkit.document import Document

from leani.pure import meta_names
from leani.queries import COMPLETE_QUERY
from leani.repl import META_NAMES, NameCompleter, config_envs
from leani.types import ConfigError


def chunk_queries(spy):
    """定数を全件たどる問い合わせの回数。重いのはこの問い合わせだけ。"""
    head = COMPLETE_QUERY.split("%s")[0]
    return sum(1 for c in spy.call_args_list if c.args[0].startswith(head))


def describe_端末の_Tab_補完():

    @story("D1")
    def it_Tab_で置き換える範囲はカーソルの前の名前だけ():
        asked = []

        def names(prefix):
            asked.append(prefix)
            return ["Nat.succ"]

        got = list(
            NameCompleter(names).get_completions(
                Document("#check Nat.suc"), CompleteEvent()
            )
        )
        assert asked == ["Nat.suc"]
        assert [(g.text, g.start_position) for g in got] == [
            ("Nat.succ", -len("Nat.suc"))
        ]


def completions(text, envs=list):
    """NameCompleter が返す候補の文字列。定数名の候補は常に Nat.succ を返す。"""
    got = NameCompleter(lambda _: ["Nat.succ"], envs).get_completions(
        Document(text), CompleteEvent()
    )
    return sorted(g.text for g in got)


def describe_略記の_Tab_補完():

    @story("C8")
    def it_略記の途中なら記号を補完する():
        got = list(
            NameCompleter(lambda _: ["Nat.succ"]).get_completions(
                Document("theorem x : p \\alp"), CompleteEvent()
            )
        )
        assert [(g.text, g.start_position) for g in got] == [("α", -len("\\alp"))]

    @story("C8")
    def it_略記の途中なら定数名を問い合わせない():
        asked = []

        def names(prefix):
            asked.append(prefix)
            return []

        list(NameCompleter(names).get_completions(Document("\\to"), CompleteEvent()))
        assert asked == []

    @story("C8")
    def it_コマンドの引数の中でも略記を補完する():
        # space はどこでも略記を変換する。Tab も同じ範囲で変換する。
        assert "→" in completions(":t p \\to")
        assert "→" in completions(":! echo \\to")


def describe_shell_コマンドの_Tab_補完():

    @story("B5")
    @pytest.mark.usefixtures("place")
    def it_1_語目は_PATH_にある実行できるコマンドを補完する():
        assert completions(":! leanf") == ["oo"]
        assert completions(":!lean") == ["bar", "foo"]

    @story("B5")
    @pytest.mark.usefixtures("place")
    def it_2_語目からはファイルのパスを補完する():
        assert completions(":! cat notes.") == ["lean", "md"]
        assert completions(":! cat bin/leanf") == ["oo"]

    @story("B5")
    @pytest.mark.usefixtures("place")
    def it_1_語目でも_slash_を含めばパスを補完する():
        assert completions(":! ./notes.l") == ["ean"]

    @story("B5")
    @pytest.mark.usefixtures("place")
    def it_shell_コマンドの中では定数名を補完しない():
        assert completions(":! cat Nat.su") == []


def describe_コマンドの_Tab_補完():

    @story("H1")
    def it_コマンド名は_HELP_に載っているものを補完する():
        assert completions(":re") == ["reload", "reset", "restart"]
        assert completions(":") == list(META_NAMES)

    @story("H1")
    def it_HELP_から_コマンド名だけを取り出す():
        help_text = "  :t, :type <expr>  型\n  :{ ... :}  囲む\n  :! <cmd>\n(:l で読む)"
        assert meta_names(help_text) == ("l", "t", "type")

    @story("B4")
    @pytest.mark.usefixtures("place")
    def it_colon_l_の後は_lean_ファイルとディレクトリだけを補完する():
        assert completions(":l notes.") == ["lean"]
        assert completions(":load ") == ["bin", "notes.lean"]

    @story("B3")
    @pytest.mark.usefixtures("place")
    def it_colon_save_の後も_lean_ファイルのパスを補完する():
        assert completions(":save no") == ["tes.lean"]

    @story("G5")
    def it_colon_env_の後は設定ファイルに書いた環境の名前を補完する():
        envs = lambda: ["mathlib", "plain"]  # noqa: E731
        assert completions(":env ", envs) == ["mathlib", "plain"]
        assert completions(":env ma", envs) == ["mathlib"]

    @story("G5")
    def it_設定ファイルを読めなければ環境の名前を補完しない(mocker):
        mocker.patch("leani.repl.load_config", side_effect=ConfigError("壊れている"))
        assert config_envs() == []

    @story("D1")
    def it_colon_t_の後は定数名を補完する():
        assert completions(":t Nat.su") == ["Nat.succ"]


def describe_名前空間ごとのキャッシュ():

    @story("D1")
    def it_同じ名前空間なら_1_回しか問い合わせない(repl, mocker):
        asked = mocker.spy(repl.engine, "query")
        assert "Nat.succ" in repl.repl.complete_names("Nat.suc")
        assert chunk_queries(asked) == 1
        repl.repl.complete_names("Nat.succ_l")
        repl.repl.complete_names("Nat.ad")
        assert chunk_queries(asked) == 1, "同じ単位の候補を取得し直している"

    @story("D1")
    def it_別の名前空間なら取得し直す(repl, mocker):
        asked = mocker.spy(repl.engine, "query")
        repl.repl.complete_names("Nat.suc")
        repl.repl.complete_names("List.ma")
        assert chunk_queries(asked) == 2

    @story("D1")
    def it_宣言を実行してもキャッシュを捨てない(repl, mocker):
        repl.repl.complete_names("Nat.suc")
        repl.feed("def myOwnHelper := 1")
        # 構文の判定にも query を使うので、宣言を実行したあとから数える。
        # 環境が変わると open の一覧を問い合わせ直すが、定数を全件たどらないので
        # 数えない。
        asked = mocker.spy(repl.engine, "query")
        repl.repl.complete_names("Nat.suc")
        assert chunk_queries(asked) == 0, "宣言のたびに取得し直している"

    @story("D1", "D5")
    def it_open_した名前空間の名前も一度に取得する(repl, mocker):
        repl.feed("open Lean", "open Nat")
        asked = mocker.spy(repl.engine, "query")
        repl.repl.complete_names("Json.pa")
        assert chunk_queries(asked) == 1


def describe_候補():

    @story("D1")
    def it_自分で実行した宣言も候補に表示される(repl):
        repl.feed("def myOwnHelper := 1")
        assert repl.repl.complete_names("myOwnH") == ["myOwnHelper"]

    @story("D1")
    def it_1_文字では候補を表示しない(repl, mocker):
        # Mathlib だと `C` だけで 7.5 万件になる。leani は問い合わせもしない。
        asked = mocker.spy(repl.engine, "query")
        assert repl.repl.complete_names("N") == []
        assert asked.call_count == 0


def describe_open_した名前空間():

    @story("D5")
    def it_open_すると短い名前で補完される(repl):
        repl.feed("open Lean")
        assert "Json.parse" in repl.repl.complete_names("Json.pa")

    @story("D5")
    def it_open_しなければ短い名前では補完されない(repl):
        assert repl.repl.complete_names("Json.pa") == []

    @story("D5")
    def it_open_X_in_の効果は次の入力に残らない(repl):
        repl.feed("open Lean in #check 1")
        assert repl.repl.complete_names("Json.pa") == []

    @story("D5")
    def it_hiding_した名前は補完されない(repl):
        repl.feed("open Lean hiding Json")
        names = repl.repl.complete_names("Js")
        assert "Json" not in names
        assert "JsonNumber" in names

    @story("D5")
    def it_名前を指定して_open_したものだけが補完される(repl):
        repl.feed("open Nat (succ_le_succ)")
        names = repl.repl.complete_names("succ_le")
        assert "succ_le_succ" in names
        assert "succ_le_of_lt" not in names

    @story("D5")
    def it_protected_な名前は最後の部分だけでは補完されない(repl):
        # Nat.add_comm は protected なので `open Nat` しても add_comm とは書けない。
        repl.feed("open Nat")
        assert "add_comm" not in repl.repl.complete_names("add_com")
        assert "succ_le_succ" in repl.repl.complete_names("succ_le_s")

    @story("D5")
    def it_namespace_の中ではその名前空間の名前も短い名前で補完される(repl):
        repl.feed("namespace Lean.Json")
        assert "parse" in repl.repl.complete_names("pars")
