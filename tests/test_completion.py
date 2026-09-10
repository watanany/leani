"""Tab 補完。問い合わせ回数は mocker で数える (時間で測るとぶれるため)。

mathlib には定数が 47 万件あり、1 回の問い合わせに 1 秒かかる。名前空間ごとに
まとめて取ってキャッシュし、宣言を通すたびに捨てないことが要点。
"""


def describe_名前空間ごとのキャッシュ():

    def it_同じ名前空間なら一度しか問い合わせない(repl, mocker):
        asked = mocker.spy(repl.engine, "query")
        assert "Nat.succ" in repl.repl._names("Nat.suc")
        assert asked.call_count == 1
        repl.repl._names("Nat.succ_l")
        repl.repl._names("Nat.ad")
        assert asked.call_count == 1, "同じ塊で取り直している"

    def it_別の名前空間なら取り直す(repl, mocker):
        asked = mocker.spy(repl.engine, "query")
        repl.repl._names("Nat.suc")
        repl.repl._names("List.ma")
        assert asked.call_count == 2

    def it_宣言を通してもキャッシュを捨てない(repl, mocker):
        repl.repl._names("Nat.suc")
        repl.feed("def myOwnHelper := 1")
        # 構文の判定にも query を使うので、数えるのは宣言を通したあとから。
        asked = mocker.spy(repl.engine, "query")
        repl.repl._names("Nat.suc")
        assert asked.call_count == 0, "宣言のたびに取り直している"


def describe_候補():

    def it_自分で通した宣言も候補に出る(repl):
        repl.feed("def myOwnHelper := 1")
        assert repl.repl._names("myOwnH") == ["myOwnHelper"]

    def it_一文字では候補を出さない(repl, mocker):
        # mathlib だと `C` だけで 7.5 万件になる。問い合わせもしない。
        asked = mocker.spy(repl.engine, "query")
        assert repl.repl._names("N") == []
        assert asked.call_count == 0
