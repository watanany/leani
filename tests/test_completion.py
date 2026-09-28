"""Tab 補完。問い合わせ回数は mocker で数える (時間で測るとぶれるため)。

mathlib には定数が 47 万件あり、1 回の問い合わせに 1 秒かかる。名前空間ごとに
まとめて取ってキャッシュし、宣言を通すたびに捨てないことが要点。
"""

from conftest import story

from leani.queries import COMPLETE_QUERY


def chunk_queries(spy):
    """定数を 1 周する問い合わせの回数。重いのはこれだけ。"""
    head = COMPLETE_QUERY.split("%s")[0]
    return sum(1 for c in spy.call_args_list if c.args[0].startswith(head))


def describe_名前空間ごとのキャッシュ():

    @story("D1")
    def it_同じ名前空間なら一度しか問い合わせない(repl, mocker):
        asked = mocker.spy(repl.engine, "query")
        assert "Nat.succ" in repl.repl._names("Nat.suc")
        assert chunk_queries(asked) == 1
        repl.repl._names("Nat.succ_l")
        repl.repl._names("Nat.ad")
        assert chunk_queries(asked) == 1, "同じ塊で取り直している"

    @story("D1")
    def it_別の名前空間なら取り直す(repl, mocker):
        asked = mocker.spy(repl.engine, "query")
        repl.repl._names("Nat.suc")
        repl.repl._names("List.ma")
        assert chunk_queries(asked) == 2

    @story("D1")
    def it_宣言を通してもキャッシュを捨てない(repl, mocker):
        repl.repl._names("Nat.suc")
        repl.feed("def myOwnHelper := 1")
        # 構文の判定にも query を使うので、数えるのは宣言を通したあとから。
        # 環境が変わると open を聞き直すが、それは定数を回らないので数えない。
        asked = mocker.spy(repl.engine, "query")
        repl.repl._names("Nat.suc")
        assert chunk_queries(asked) == 0, "宣言のたびに取り直している"

    @story("D1", "D5")
    def it_open_した名前空間の分も一度に取る(repl, mocker):
        repl.feed("open Lean", "open Nat")
        asked = mocker.spy(repl.engine, "query")
        repl.repl._names("Json.pa")
        assert chunk_queries(asked) == 1


def describe_候補():

    @story("D1")
    def it_自分で通した宣言も候補に出る(repl):
        repl.feed("def myOwnHelper := 1")
        assert repl.repl._names("myOwnH") == ["myOwnHelper"]

    @story("D1")
    def it_一文字では候補を出さない(repl, mocker):
        # mathlib だと `C` だけで 7.5 万件になる。問い合わせもしない。
        asked = mocker.spy(repl.engine, "query")
        assert repl.repl._names("N") == []
        assert asked.call_count == 0


def describe_open_した名前空間():

    @story("D5")
    def it_open_すると短い名前で出る(repl):
        repl.feed("open Lean")
        assert "Json.parse" in repl.repl._names("Json.pa")

    @story("D5")
    def it_open_しなければ短い名前では出ない(repl):
        assert repl.repl._names("Json.pa") == []

    @story("D5")
    def it_open_X_in_は後に残らない(repl):
        repl.feed("open Lean in #check 1")
        assert repl.repl._names("Json.pa") == []

    @story("D5")
    def it_hiding_した名前は出ない(repl):
        repl.feed("open Lean hiding Json")
        names = repl.repl._names("Js")
        assert "Json" not in names
        assert "JsonNumber" in names

    @story("D5")
    def it_名前を選んで_open_した分だけ出る(repl):
        repl.feed("open Nat (succ_le_succ)")
        names = repl.repl._names("succ_le")
        assert "succ_le_succ" in names
        assert "succ_le_of_lt" not in names

    @story("D5")
    def it_protected_な名前は最後の語だけでは出ない(repl):
        # Nat.add_comm は protected なので `open Nat` しても add_comm とは書けない。
        repl.feed("open Nat")
        assert "add_comm" not in repl.repl._names("add_com")
        assert "succ_le_succ" in repl.repl._names("succ_le_s")

    @story("D5")
    def it_namespace_の中ではその名前空間の名前も短く出る(repl):
        repl.feed("namespace Lean.Json")
        assert "parse" in repl.repl._names("pars")
