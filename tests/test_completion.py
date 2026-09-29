"""Tab 補完。問い合わせ回数は mocker で数える (時間で測ると結果が安定しないため)。

Mathlib には定数が 47 万件あり、1 回の問い合わせに 1 秒かかる。名前空間ごとに
まとめて取得してキャッシュし、宣言を実行するたびにキャッシュを捨てないことが要点。
"""

from conftest import story

from leani.queries import COMPLETE_QUERY


def chunk_queries(spy):
    """定数を全件たどる問い合わせの回数。重いのはこの問い合わせだけ。"""
    head = COMPLETE_QUERY.split("%s")[0]
    return sum(1 for c in spy.call_args_list if c.args[0].startswith(head))


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
