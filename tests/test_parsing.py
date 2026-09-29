"""入力をどう解釈するかを決める純関数。端末もエンジンも不要なのでミリ秒で終わる。"""

import io
import os
import subprocess
import sys

import pytest
from conftest import ROOT, leani, story
from hypothesis import given
from hypothesis import strategies as st

# ----------------------------------------------- property test のデータ

# 括弧が閉じている文字列。中身は Lean のコードに似せてあるが、
# テストに必要なのは括弧の閉じ方だけ。
BALANCED = st.recursive(
    st.sampled_from(["", "x", "exact rfl", "simp only"]),
    lambda inner: st.one_of(
        st.tuples(inner, inner).map("".join),
        st.tuples(st.sampled_from(sorted(leani.pure.PAIRS)), inner).map(
            lambda pair: pair[0] + pair[1] + leani.pure.PAIRS[pair[0]]
        ),
    ),
    max_leaves=8,
)

# sorry の前後に置く行。コメントや文字列の中に "sorry" を書いた行を混ぜてある。
DECOYS = ["", "def f : True := by", "-- sorry はここにもある", '"sorry"', "  rfl"]

# ヘッダ部分のデータ。import、コメント、本体と、import に見えるが import ではない行。
HEADERS = [
    "import Std",
    "import Lean.Elab",
    "  import Mathlib",
    "import",
    "import Foo -- メモ",
    "-- import Bar",
    "/- ここから",
    "import Baz",
    "ここまで -/",
    "",
    "def a := 1",
]


def describe_継続行の判定():
    """単独では入力になりえない行を見分ける。"""

    @story("C1")
    def it_インデントした行は前の行の続き():
        for line in ("  | 0 => 0", "\tfoo", "  bar"):
            assert leani.pure.continues(line), f"継続にならない: {line!r}"

    @story("C1")
    def it_行頭のパイプは前の行の続き():
        # `|` で始まる command も tactic も Lean には無い。
        assert leani.pure.continues("| zero => rfl")

    @story("A1", "C1")
    def it_ふつうの行は単独の入力():
        for line in ("def f := 1", "#eval 1", "", "x + 1"):
            assert not leani.pure.continues(line), f"継続にされた: {line!r}"


def describe_続きを待つ行():
    """完結していても次の行がありうる構文。"""

    @story("C1")
    def it_where_with_do_by_で終わる行は空行を待つ():
        for line in (
            "structure P where",
            "induction n with",
            "def f := do",
            "theorem t : True := by",
        ):
            assert leani.queries.BLOCK_OPEN.search(line), f"待たない: {line!r}"

    @story("C1")
    def it_語の一部として含むだけなら待たない():
        for line in ("def where_ := 1", "#eval byte", "def f := 1"):
            assert not leani.queries.BLOCK_OPEN.search(line), f"余計に待つ: {line!r}"


def describe_パーサからの応答():
    """「まだ途中」という応答だけを継続とみなす。"""

    @story("C1")
    def it_入力の途中なら次の行を待つ():
        assert leani.queries.INCOMPLETE.search("<input>:1:5: unexpected end of input")
        assert leani.queries.INCOMPLETE.search("unterminated comment")

    @story("C1")
    def it_ただのエラーは待たずに送る():
        assert not leani.queries.INCOMPLETE.search("unknown identifier 'foo'")


def describe_タクティクの提案():
    """exact? や simp? は結果を "Try this:" として返す。"""

    @story("E2")
    def it_提案から項だけを取り出す():
        found = leani.pure.try_this(
            [{"severity": "info", "data": "Try this:\n  [apply] exact Nat.le_refl n"}]
        )
        assert found == "exact Nat.le_refl n"

    @story("E2")
    def it_折り返した提案を全部取り出す():
        # pretty printer が 100 桁前後で折り返す。1 行目だけ取り出すと
        # `simp only [a, b,` になり、それが完成した証明として表示されてしまう。
        found = leani.pure.try_this(
            [
                {
                    "severity": "info",
                    "data": "Try this:\n  [apply] simp only [aaa, bbb,\n    ccc, ddd]",
                }
            ]
        )
        assert found == "simp only [aaa, bbb,\n  ccc, ddd]"

    @story("E2")
    def it_Try_this_と同じ行に書かれた提案も取り出す():
        found = leani.pure.try_this([{"data": "Try this: exact Nat.le_refl n"}])
        assert found == "exact Nat.le_refl n"

    @story("E2")
    def it_括弧が合わない提案は途中で切れていると判定する():
        # スクリプトに追加する前の最後の確認。途中で切ると必ず括弧が合わなくなる。
        assert leani.pure.balanced("simp only [aaa, bbb]")
        assert leani.pure.balanced("exact ⟨foo (bar x), rfl⟩")
        assert not leani.pure.balanced("simp only [aaa, bbb,")
        assert not leani.pure.balanced("exact foo)")
        assert not leani.pure.balanced("exact ⟨foo]")

    @story("E2")
    @given(
        a=BALANCED, b=BALANCED, close=st.sampled_from(list(leani.pure.PAIRS.values()))
    )
    def it_閉じた文字列同士を連結しても閉じている(a, b, close):
        # 例では 5 通りしか書けない。閉じた文字列を文法から生成して、連結しても
        # 閉じたままであることと、閉じ括弧を 1 つ足すと必ず閉じなくなることを確認する。
        assert leani.pure.balanced(a)
        assert leani.pure.balanced(a + b)
        assert not leani.pure.balanced(a + close)

    @story("E2")
    def it_提案でなければ何も返さない():
        assert (
            leani.pure.try_this([{"severity": "info", "data": "goals accomplished"}])
            is None
        )
        assert leani.pure.try_this([]) is None


def describe_宣言の名前の取り出し():
    """ユーザーが実行した宣言を補完の候補に追加するために使う。"""

    @story("D1")
    def it_修飾子や属性が付いていても名前を取り出せる():
        cases = {
            "def foo := 1": "foo",
            "theorem bar (n : Nat) : n = n := rfl": "bar",
            "@[simp] private noncomputable def baz : Nat := 0": "baz",
            "structure Point where\n  x : Nat": "Point",
        }
        for src, want in cases.items():
            assert leani.queries.DECL_NAME.findall(src) == [want], f"取れない: {src!r}"

    @story("D1")
    def it_宣言でないものからは名前を取り出さない():
        assert leani.queries.DECL_NAME.findall("#eval 1 + 1") == []


def describe_補完の候補をまとめて取得する単位():
    """名前空間があればそこまで、無ければ先頭 2 文字。"""

    @story("D1")
    def it_名前空間の区切りで分ける():
        assert leani.repl.Repl._chunk("Nat.suc") == "Nat."
        assert leani.repl.Repl._chunk("MeasureTheory.integral_") == "MeasureTheory."

    @story("D1")
    def it_名前空間が無ければ先頭_2_文字():
        # Mathlib では 1 文字だと `C` で 7.5 万件になるので、単位を広げすぎない。
        assert leani.repl.Repl._chunk("Contin") == "Co"


def describe_入力が完結したかの判定():
    """パーサの応答 (JSON) だけを見て、送り方と状態を決める。"""

    @story("A1", "C1")
    def it_command_として読めたらそのまま送る():
        probe = {"cmd": {"ok": True}, "term": {"ok": False, "err": "x"}}
        assert leani.pure.classify(probe) == (leani.types.COMPLETE, leani.types.CMD)

    @story("A1")
    def it_term_としてしか読めなければ_eval_に包む():
        probe = {"cmd": {"ok": False, "err": "x"}, "term": {"ok": True}}
        assert leani.pure.classify(probe) == (leani.types.COMPLETE, leani.types.TERM)

    @story("C1")
    def it_途中なら次の行を待つ():
        probe = {
            "cmd": {"ok": False, "err": "<input>:1:9: unexpected end of input"},
            "term": {"ok": False, "err": "<input>:1:3: unexpected token"},
        }
        assert leani.pure.classify(probe) == (leani.types.MORE, leani.types.CMD)

    @story("C1")
    def it_より先まで解析できたほうをユーザーの意図とみなす():
        probe = {
            "cmd": {"ok": False, "err": "<input>:1:2: unexpected token"},
            "term": {"ok": False, "err": "<input>:1:7: unexpected token"},
        }
        assert leani.pure.classify(probe) == (leani.types.ERR, leani.types.TERM)

    @story("C1")
    def it_判定できなければ_Lean_に本当のエラーを出させる():
        # leani の推測でエラーを作らない。command として送る。
        assert leani.pure.classify(None) == (leani.types.ERR, leani.types.CMD)

    @story("E1")
    def it_証明モードでは_tacticSeq_だけを見る():
        assert leani.pure.classify_tac({"tac": {"ok": True}}) == (
            leani.types.COMPLETE,
            leani.types.TAC,
        )
        assert leani.pure.classify_tac(
            {"tac": {"ok": False, "err": "unexpected end of input"}}
        ) == (leani.types.MORE, leani.types.TAC)
        assert leani.pure.classify_tac(None) == (leani.types.ERR, leani.types.TAC)

    @story("C1")
    def it_インデントで続いたブロックは空行まで待つ():
        assert leani.pure.block_continues(["def f", "  | 0 => 0"], "def f\n  | 0 => 0")
        assert not leani.pure.block_continues(["def f := 1"], "def f := 1")


def describe_エラー位置の範囲():
    """メッセージの位置を、送ったソースの座標からその行の中に収める。"""

    @story("A2")
    def it_eval_で包んだ分だけ位置を戻す():
        m = {"pos": {"line": 2, "column": 6}, "endPos": {"line": 2, "column": 9}}
        # #eval で 1 行 2 桁ずらして送っているので、元のソースでは 1 行 4 桁。
        assert leani.pure.span(m, ["foo bar"], line_off=1, col_off=2) == (1, 4, 3)

    @story("A2")
    def it_位置が行の外なら範囲を返さない():
        m = {"pos": {"line": 9, "column": 0}}
        assert leani.pure.span(m, ["foo"], 0, 0) is None

    @story("A2")
    def it_範囲の幅は行の長さに収める():
        m = {"pos": {"line": 1, "column": 1}, "endPos": {"line": 1, "column": 99}}
        assert leani.pure.span(m, ["abc"], 0, 0) == (1, 1, 2)


def describe_レスポンスの切り出し():
    """空行で区切られた JSON を、受信し終えた分だけ取り出す。"""

    @story("F1")
    def it_受信し終えるまでは何も返さない():
        assert leani.pure.first_response('{"env": 1}') is None

    @story("F1")
    def it_空行を受け取ったら読み込む():
        assert leani.pure.first_response('{"env": 1}\n\n') == {"env": 1}

    @story("F1")
    def it_JSON_の中の空行では区切らない():
        buf = '{"messages": [{"data": "a\\n\\nb"}]}\n\n'
        assert leani.pure.first_response(buf)["messages"][0]["data"] == "a\n\nb"


def describe_起動前の準備():
    """import を追加したり取り除いたりする処理。"""

    @story("G2")
    def it_init_ファイルの_import_行は取り除く():
        # init は既存の環境の上に追加するので、import を書けない。
        assert leani.pure.strip_imports("import Foo\ndef x := 1") == "def x := 1"

    @story("G2")
    def it_import_を含むファイルには_import_を追加しない():
        assert leani.pure.has_import("\nimport Mathlib\ndef f := 1")
        assert not leani.pure.has_import("def f := 1  -- import ではない")

    @story("G1")
    def it_lake_env_のキャッシュを読む():
        env = leani.pure.parse_env_lines("LEAN_PATH=/a:/b\nLD_LIBRARY_PATH=\n")
        assert env == {"LEAN_PATH": "/a:/b", "LD_LIBRARY_PATH": ""}

    @story("G3", "G4")
    def it_toolchain_が変わったらキャッシュを作り直す(tmp_path):
        # LEAN_PATH は core の .olean も指す。前のバージョンのキャッシュを再利用すると、
        # repl は起動するのに import がすべて失敗して、原因が分からなくなる。
        project = tmp_path / "proj"
        project.mkdir()
        (project / "lake-manifest.json").write_text("{}")
        cache = tmp_path / "cache"
        cache.write_text("LEAN_PATH=old\n")

        os.utime(project / "lake-manifest.json", (1, 1))
        os.utime(cache, (2, 2))
        assert not leani.boot.lake_env_stale(str(cache), str(project))

        (project / "lean-toolchain").write_text("leanprover/lean4:v4.33.1\n")
        os.utime(project / "lean-toolchain", (3, 3))
        assert leani.boot.lake_env_stale(str(cache), str(project))


def describe_エンジンのバージョン():
    """使う Lean のバージョンから repl のタグとパスを決める。"""

    @story("G3")
    def it_toolchain_からバージョンだけ取る():
        assert (
            leani.pure.toolchain_version("leanprover/lean4:v4.34.0-rc2")
            == "v4.34.0-rc2"
        )
        assert leani.pure.toolchain_version("v4.33.0") == "v4.33.0"

    @story("G3")
    def it_rc_は同じバージョンの正式リリースより前():
        assert leani.pure.version_key("v4.33.0-rc1") < leani.pure.version_key("v4.33.0")
        assert leani.pure.version_key("v4.33.0") < leani.pure.version_key("v4.34.0-rc1")

    @story("G3")
    def it_解釈できないバージョンは_None():
        for tag in ("v4.33", "nightly-2026-01-01", "4.33.0"):
            assert leani.pure.version_key(tag) is None, f"解釈できてしまう: {tag!r}"

    @story("G3")
    def it_同名のタグがあればそれを使う():
        tags = ["v4.32.0", "v4.33.0", "v4.34.0-rc2"]
        assert leani.pure.pick_tag("v4.33.0", tags) == "v4.33.0"

    @story("G3")
    def it_タグの無い_patch_リリースは直前のタグを使う():
        # repl は v4.33.1 にタグを付けない。API は patch で変わらない。
        tags = ["v4.32.0", "v4.33.0", "v4.34.0-rc2"]
        assert leani.pure.pick_tag("v4.33.1", tags) == "v4.33.0"

    @story("G3")
    def it_どのタグより古いバージョンなら_None():
        assert leani.pure.pick_tag("v4.0.0", ["v4.32.0", "v4.33.0"]) is None
        assert leani.pure.pick_tag("nightly", ["v4.33.0"]) is None

    @story("G3")
    def it_パスはバージョンごとに分かれる():
        got = leani.pure.engine_dir(None, "leanprover/lean4:v4.33.0")
        assert got == f"{leani.places.ENGINE_CACHE}/v4.33.0"

    @story("G3")
    def it_明示されたパスはそのまま使う():
        assert (
            leani.pure.engine_dir("/opt/repl", "leanprover/lean4:v4.33.0")
            == "/opt/repl"
        )

    @story("G3")
    def it_パスの外を指すバージョン名は受け付けない():
        # build_engine はこの場所を rmtree してから作り直す。`..` を含む名前を
        # 受け付けると、関係ないディレクトリを消してしまう。
        for tc in ("leanprover/lean4:..", "..", "."):
            with pytest.raises(leani.types.EngineError):
                leani.pure.engine_dir(None, tc)


def describe_設定の値の型():
    """TOML には何でも書ける。型が違う値は traceback ではなく ConfigError にする。"""

    @story("G4")
    def it_import_を文字列で書いたらエラーにする():
        # エラーにせず受け付けると 1 文字ずつの import になり、どれも解決できないので
        # ヘッダがすべて捨てられる (import Lean も消える)。
        cfg = {"default": "m", "env": {"m": {"imports": "Mathlib"}}}
        with pytest.raises(leani.types.ConfigError, match="配列"):
            leani.config.resolve(cfg=cfg)

    @story("G4")
    def it_文字列で書くべき値が別の型ならエラーにする():
        for key, value in (("project", 3), ("engine", True), ("prompt", 5)):
            cfg = {"default": "m", "env": {"m": {key: value}}}
            with pytest.raises(leani.types.ConfigError, match=key):
                leani.config.resolve(cfg=cfg)

    @story("G4")
    def it_表で書くべき値が別の型ならエラーにする():
        with pytest.raises(leani.types.ConfigError, match="env は表"):
            leani.config.resolve(cfg={"default": "m", "env": 3})
        with pytest.raises(leani.types.ConfigError, match=r"env\.m は表"):
            leani.config.resolve(cfg={"default": "m", "env": {"m": "nope"}})

    @story("G4")
    def it_default_が文字列でなければエラーにする():
        with pytest.raises(leani.types.ConfigError, match="default"):
            leani.config.resolve(cfg={"default": 3, "env": {"m": {}}})

    @story("G4", "G5")
    def it_書き方が正しければ値をそのまま読み込む():
        cfg = {"default": "m", "env": {"m": {"imports": ["A", "B"], "prompt": "> "}}}
        got = leani.config.resolve(cfg=cfg)
        assert (got.name, got.imports, got.prompt) == ("m", ("A", "B"), "> ")


def _toolchain_dir(tmp_path, name, tc):
    d = tmp_path / name
    d.mkdir()
    (d / "lean-toolchain").write_text(tc + "\n")
    return str(d)


def describe_使うバージョンの決め方():
    """
    elan run に渡すバージョン。

    エンジンをビルドしたバージョンと合わないと .olean を読み込めない。
    """

    @story("G3")
    def it_プロジェクトのバージョンが最優先(tmp_path):
        cfg = leani.config.EnvConfig.make(
            "t",
            project=_toolchain_dir(tmp_path, "proj", "leanprover/lean4:v4.34.0-rc2"),
            engine=_toolchain_dir(tmp_path, "eng", "leanprover/lean4:v4.33.0"),
        )
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:v4.34.0-rc2"

    @story("G3")
    def it_明示したエンジンのバージョンを使う(tmp_path):
        # ユーザーがビルドしたエンジンは leani がビルドし直さないので、そのバージョンに
        # 合わせるしかない。
        # elan のデフォルトのバージョンを使うと、4.34 の .olean を 4.33 で
        # 読み込むことになる。
        cfg = leani.config.EnvConfig.make(
            "t", engine=_toolchain_dir(tmp_path, "eng", "leanprover/lean4:v4.33.0")
        )
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:v4.33.0"

    @story("G3")
    def it_どちらも無ければ_elan_のデフォルト(mocker):
        mocker.patch.object(
            leani.boot, "local_toolchain", return_value="leanprover/lean4:v4.9.0"
        )
        cfg = leani.config.EnvConfig.make("t")
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:v4.9.0"


def describe_手動でエンジンを用意する手順():
    """
    leani が自動で用意できなかったときに表示する。書いてあるとおりに実行すると、
    自動で用意したものと同じものができる。
    """

    @story("G3")
    def it_タグとバージョンの書き換えまで含む():
        out = leani.boot.manual_setup("/x/e", "leanprover/lean4:v4.33.1", "v4.33.0")
        assert "--branch v4.33.0" in out, out
        assert "leanprover/lean4:v4.33.1 > /x/e/lean-toolchain" in out, out

    @story("G3")
    def it_タグが分からなければ選び方を書く():
        out = leani.boot.manual_setup("/x/e", "leanprover/lean4:v4.33.1", None)
        assert "v4.33.1 以下で一番新しいタグ" in out, out


def _fake_setup(seen):
    """clone と build の代わりをする。clone 先を seen に記録する。"""

    def run(cmd, cwd=None):
        if cmd[0] == "git":
            seen.append(cmd[-1])
            os.makedirs(cmd[-1])
        else:
            os.makedirs(f"{cwd}/.lake/build/bin")
            open(f"{cwd}/.lake/build/bin/repl", "w").close()

    return run


def describe_エンジンを用意するときの安全対策():
    """
    leani が削除してよいのは、leani 自身が作ったディレクトリだけ。
    leani を 2 つ同時に実行してもよい。
    """

    @story("G3")
    def it_明示されたエンジンは作り直さない(tmp_path, mocker):
        # build_engine は置き場所を作り直すので、ユーザーが指定した場所に対して
        # 実行してはならない。
        build = mocker.patch.object(leani.boot, "build_engine")
        eng = _toolchain_dir(tmp_path, "eng", "leanprover/lean4:v4.33.0")
        with pytest.raises(leani.types.EngineError, match="未ビルド"):
            leani.boot.ensure_engine(eng, "leanprover/lean4:v4.33.0")

        assert not build.called
        assert os.path.isfile(f"{eng}/lean-toolchain")

    @story("G3", "G4")
    def it_明示されたエンジンにバージョンが無ければ起動を中止する(tmp_path, mocker):
        # 合わせられるバージョンはこのエンジンのものしかない。何も表示せずに elan の
        # デフォルトを使うと .olean を読み込めない。
        mocker.patch.object(leani.config.shutil, "which", return_value="/usr/bin/x")
        (tmp_path / "eng/.lake/build/bin").mkdir(parents=True)
        (tmp_path / "eng/.lake/build/bin/repl").touch()
        cfg = leani.config.EnvConfig.make("t", engine=str(tmp_path / "eng"))
        assert "lean-toolchain" in (leani.config.problem(cfg) or "")

    @story("G3")
    def it_作業中の置き場所はプロセスごとに分かれる(tmp_path, mocker):
        # leani を 2 つ同時に起動したとき、同じ .tmp を使うと片方が起動できない。
        seen = []
        mocker.patch.object(leani.boot, "run_setup", side_effect=_fake_setup(seen))
        path = str(tmp_path / "engine" / "v4.33.0")
        leani.boot.build_engine(path, "leanprover/lean4:v4.33.0", "v4.33.0")

        assert str(os.getpid()) in seen[0], seen
        assert os.path.isfile(f"{path}/.lake/build/bin/repl")
        assert not os.path.exists(seen[0]), "作業中の置き場所が残っている"

    @story("G3")
    def it_待っているあいだに別の_leani_が置いたエンジンは消さない(tmp_path, mocker):
        # 消すと、そのエンジンを使っている別の leani が動かなくなる。
        path = str(tmp_path / "engine" / "v4.33.0")
        os.makedirs(f"{path}/.lake/build/bin")
        with open(f"{path}/.lake/build/bin/repl", "w") as f:
            f.write("先にあったもの")

        seen = []
        mocker.patch.object(leani.boot, "run_setup", side_effect=_fake_setup(seen))
        leani.boot.build_engine(path, "leanprover/lean4:v4.33.0", "v4.33.0")

        with open(f"{path}/.lake/build/bin/repl") as f:
            assert f.read() == "先にあったもの"

        assert not os.path.exists(seen[0])


POS_OUT = {"pos": {"line": 99, "column": 0}, "endPos": {"line": 99, "column": 5}}


def mark(src, word="sorry", nth=0):
    """src の中の word を repl と同じ {line, column} で指す。桁は codepoint 単位。"""
    at = -1
    for _ in range(nth + 1):
        at = src.index(word, at + 1)
    line = src.count("\n", 0, at) + 1
    col = at - (src.rfind("\n", 0, at) + 1)
    return {
        "pos": {"line": line, "column": col},
        "endPos": {"line": line, "column": col + len(word)},
    }


def describe_sorry_の置き換え():
    """
    repl の pos / endPos で切り出してスクリプトに置き換える。桁は codepoint 単位で、
    Python のインデックスと一致する (日本語のコメントを含めて実際に確認した)。
    """

    @story("E3")
    def it_日本語を含んでも位置で切り出せる():
        src = "theorem a : True := by /- あいう -/ sorry"
        got = leani.pure.splice_sorry(src, mark(src), "trivial")
        assert got == "theorem a : True := by /- あいう -/ trivial", got

    @story("E3")
    def it_単独の行にある_sorry_のインデントを変えない():
        # 空白を詰めるとインデントが浅くなり、内側の by ブロックの外に出てしまう。
        src = "example : True := by\n  have h : True := by\n    sorry\n  exact h"
        got = leani.pure.splice_sorry(src, mark(src), "trivial")
        assert got == (
            "example : True := by\n  have h : True := by\n    trivial\n  exact h"
        ), got

    @story("E3")
    def it_複数行のスクリプトは_sorry_の桁にそろえる():
        src = "example : True := by\n  have h : True := by\n    sorry\n  exact h"
        got = leani.pure.splice_sorry(src, mark(src), "constructor\n-- おわり")
        assert got == (
            "example : True := by\n"
            "  have h : True := by\n"
            "    constructor\n"
            "    -- おわり\n"
            "  exact h"
        ), got

    @story("E3")
    def it_行の途中の_sorry_を複数行のスクリプトにするときは_by_の次の行に下げる():
        src = "theorem t (n : Nat) : n + 0 = n := by sorry"
        got = leani.pure.splice_sorry(src, mark(src), "induction n with\n| zero => rfl")
        assert got == (
            "theorem t (n : Nat) : n + 0 = n := by\n  induction n with\n  | zero => rfl"
        ), got

    @story("E3")
    def it_匿名コンストラクタの中の_sorry_に空白を足さない():
        src = "example : True ∧ True := ⟨by sorry, by trivial⟩"
        got = leani.pure.splice_sorry(src, mark(src), "trivial")
        assert got == "example : True ∧ True := ⟨by trivial, by trivial⟩", got

    @story("E3")
    def it_位置が_sorry_を指していなければ置き換えない():
        src = "example : True := by trivial"
        # 位置は合っているが指しているのは sorry ではない / 行が無い。
        assert leani.pure.splice_sorry(src, mark(src, "trivial"), "rfl") is None
        assert leani.pure.splice_sorry(src, POS_OUT, "rfl") is None
        assert (
            leani.pure.splice_sorry(src, {"pos": None, "endPos": None}, "trivial")
            is None
        )

    @story("E3")
    @given(
        lines=st.lists(st.text(alphabet="ab \t", max_size=4), min_size=1, max_size=5),
        n=st.integers(min_value=0, max_value=99),
        col=st.integers(min_value=0, max_value=99),
    )
    def it_行と桁を文字のインデックスに変換できる(lines, n, col):
        # splice_sorry が切る位置はこれで決まる。1 桁ずれると sorry の途中で
        # 切ってしまう。
        src = "\n".join(lines)
        n %= len(lines)
        col %= len(lines[n]) + 1
        i = leani.pure.offset(src, {"line": n + 1, "column": col})
        assert i is not None
        assert src[i:].split("\n")[0] == lines[n][col:]

    @story("E3")
    @given(
        before=st.lists(st.sampled_from(DECOYS), max_size=4),
        pad=st.sampled_from(["", "  ", "    ", "  exact ⟨"]),
        after=st.lists(st.sampled_from(DECOYS), max_size=4),
        script=st.sampled_from(["rfl", "induction n with\n| zero => rfl"]),
    )
    def it_sorry_の前後は書き換えない(before, pad, after, script):
        # 前後にコメントとして "sorry" を混ぜる。"sorry" の出現を数えて位置を
        # 決めていると、そちらを切ってしまう。位置で切り出している限り、前後は
        # 1 文字も変わらない。
        src = "\n".join([*before, pad + "sorry", *after])
        line = len(before) + 1
        sy = {
            "pos": {"line": line, "column": len(pad)},
            "endPos": {"line": line, "column": len(pad) + len("sorry")},
        }
        a = leani.pure.offset(src, sy["pos"])
        b = leani.pure.offset(src, sy["endPos"])
        got = leani.pure.splice_sorry(src, sy, script)

        assert got is not None
        assert got.endswith(src[b:])
        # 行の途中にある sorry を複数行のスクリプトで置き換えるときは、sorry の手前の
        # 空白を取り除き、スクリプトを by の次の行から書く。
        assert got.startswith(src[:a].rstrip(" \t"))


def describe_ファイルの_import():
    """:save のヘッダに書き戻すため、読み込んだファイルの import を覚える。"""

    @story("B3", "B4")
    def it_import_の行だけを取り出す():
        src = "import Lean.Elab\nimport  Std\n\ndef a := 1\n-- import Nope\n"
        assert leani.pure.import_lines(src) == ["Lean.Elab", "Std"]

    @story("B3")
    def it_import_が無ければ空():
        assert leani.pure.import_lines("def a := 1\nimport\n") == []

    @story("B3", "B4")
    def it_コメントの中の_import_は数えない():
        # ヘッダに書くと repl はヘッダ全体を捨てて起動するので、書き出したファイルは
        # :l でも lean でもエラーになる。import 行を取り除く処理も doc コメントを壊す。
        src = "/-\nimport Nope.NotAModule\n-/\n-- import Also.Not\nimport Std\n"
        assert leani.pure.import_lines(src) == ["Std"]
        assert "import Nope.NotAModule" in leani.pure.strip_imports(src)
        assert "import Std" not in leani.pure.strip_imports(src)

    @story("B3")
    def it_宣言のあとの_import_は数えない():
        # Lean も先頭以外の import を認めない。数えると :save のヘッダが増える。
        assert leani.pure.import_lines("def a := 1\nimport Std\n") == []

    @story("B4")
    def it_入れ子のコメントの終わりを正しく判定する():
        src = "/- /- import In.Nest -/ import Still.In -/\nimport Real\n"
        assert leani.pure.import_lines(src) == ["Real"]

    @story("B3", "B4")
    @given(lines=st.lists(st.sampled_from(HEADERS), max_size=12))
    def it_取り除くのは本物の_import_行だけ(lines):
        src = "\n".join(lines)
        mods, at = leani.pure.scan_header(src)
        stripped = leani.pure.strip_imports(src)

        assert len(mods) == len(at)
        # コメントの中の import を取り出していない。取り出すと :save した
        # ファイルが壊れる。
        assert all(lines[n].split()[:1] == ["import"] for n in at)
        # 取り除いたあとに新しい import 行が現れることはない。ヘッダの終わりは
        # 変わらないので、1 回ですべて取り除ける。
        assert leani.pure.import_lines(stripped) == []


def describe_略記の展開():
    """
    `\name` を Lean の記号に変換する。変換は space で確定し、キー入力のたびには
    確認しない。
    """

    @story("C5")
    def it_名前を記号に変換する():
        assert leani.abbrev.expand_abbrev("a \\to") == ("→", 3)
        assert leani.abbrev.expand_abbrev("x\\dot") == ("·", 4)

    @story("C5")
    def it_長い略記が短い略記として変換されない():
        # `\a` も `\all` も `\alpha` も表にある。キー入力のたびに確定すると `\a` の
        # 時点で変換されてしまい、長いほうを入力できなくなる。
        assert leani.abbrev.expand_abbrev("\\a") == ("α", 2)
        assert leani.abbrev.expand_abbrev("\\all") == ("∀", 4)
        assert leani.abbrev.expand_abbrev("\\alpha") == ("α", 6)

    @story("C5")
    def it_めったに使わない略記も変換できる():
        # 表には VS Code の Lean 拡張の略記をほぼすべて (1829 件) 含めている。
        assert leani.abbrev.expand_abbrev("\\frown") == ("⌢", 6)
        assert leani.abbrev.expand_abbrev("\\Gangia") == ("Ϫ", 7)

    @story("C5")
    def it_表に無い名前は変えない():
        assert leani.abbrev.expand_abbrev("\\nosuch") is None
        assert leani.abbrev.expand_abbrev("1 + 1") is None

    @story("C5")
    def it_略記のあとに記号が続く場合は変換しない():
        # `\to)` 全体で表を検索する。`"\t"` のような文字列を ▸ に変換しない。
        assert leani.abbrev.expand_abbrev("(\\to)") is None
        assert leani.abbrev.expand_abbrev('"\\t"') is None

    @story("C5")
    @given(
        key=st.sampled_from(sorted(leani.abbrev.ABBREV)),
        head=st.text(alphabet=st.characters(exclude_characters="\\"), max_size=8),
    )
    def it_表のどの略記も変換できる(key, head):
        # 例に書けるのは数件。1829 件すべてが同じ規則で変換できることはここで確認する。
        # 入力位置の手前に何が書いてあっても、直前の `\\` より後ろだけをキーにする。
        want = (leani.abbrev.ABBREV[key], len(key) + 1)
        assert leani.abbrev.expand_abbrev(head + "\\" + key) == want


def describe_定理検索():
    """
    loogle に問い合わせた結果を表示用の形に整形する。
    外部と通信する部分だけが副作用を持ち、整形は純関数で行う。
    """

    HITS = [
        {
            "name": "List.map",
            "type": " (f : α → β) : List α → List β",
            "module": "Init.Prelude",
        },
        {
            "name": "List.mapTR",
            "type": " (f : α → β) : List α → List β",
            "module": "Init.Data.List.Basic",
        },
    ]

    @story("D4")
    def it_名前と型と定義されている_module_を表示する():
        out = leani.pure.loogle_text({"count": 2, "hits": HITS}).split("\n")
        assert out[0] == "2 件"
        # `type` は先頭に空白が付いているので、`name : type` の形に整形する。
        assert out[1] == "List.map : (f : α → β) : List α → List β"
        # どの module にあるかも表示する。手元の環境に無い名前も結果に含まれるため。
        assert out[2] == "  Init.Prelude"
        assert out[3].startswith("List.mapTR : ")

    @story("D4")
    def it_件数を絞ったときは全体の件数も表示する():
        # loogle は結果を 200 件で打ち切って返すので、count は hits より多いことがある。
        got = {"count": 360, "hits": HITS}
        assert (
            leani.pure.loogle_text(got, keep=1).split("\n")[0] == "360 件 (先頭 1 件)"
        )
        assert leani.pure.loogle_text({"count": 2, "hits": HITS}).count("\n") == 4

    @story("D4")
    def it_長い行は端末の幅で切り詰める():
        # 折り返すと 1 件 2 行の並びが崩れて、どの型がどの名前のものか分からない。
        got = {
            "count": 1,
            "hits": [{"name": "X", "type": " " + "a" * 200, "module": "M"}],
        }
        line = leani.pure.loogle_text(got, width=40).split("\n")[1]
        assert len(line) == 40
        assert line.endswith("…")

    @story("D4")
    def it_見つからなければその旨を表示する():
        assert leani.pure.loogle_text({"count": 0, "hits": []}) == "見つからなかった"

    @story("D4")
    def it_エラーは翻訳せずに候補を添える():
        # loogle の文言は Lean のパーサのもの。日本語にすると元の位置情報が消える。
        got = {"error": "unknown identifier 'Nope'", "suggestions": ['"Nope"']}
        out = leani.pure.loogle_text(got).split("\n")
        assert out == ["loogle: unknown identifier 'Nope'", 'もしかして: "Nope"']

    @story("D4")
    def it_記号を含むクエリも壊さずに送る(mocker):
        # `?a + ?b` の `+` をそのまま URL に含めると、loogle 側では空白として
        # 解釈される。
        opened = mocker.patch(
            "urllib.request.urlopen",
            return_value=io.BytesIO(b'{"count": 0, "hits": []}'),
        )
        leani.search.loogle("?a + ?b")
        assert opened.call_args[0][0].endswith("?q=%3Fa%20%2B%20%3Fb")

    @story("D4")
    def it_応答が無ければ_SearchError(mocker):
        # ネットワークの問題で REPL を終了させない。呼び出し側が例外を受け取って
        # 1 行で報告する。
        mocker.patch("urllib.request.urlopen", side_effect=OSError("名前が引けない"))
        with pytest.raises(leani.types.SearchError, match="名前が引けない"):
            leani.search.loogle("Nat")


def describe_性質一覧():
    """
    SPEC.md はテスト名から生成する (tools/spec.py)。手で書き足す場所を
    作らない代わりに、生成し忘れをこのテストで検出する。
    """

    @story("X1")
    def it_SPEC_md_がテストとそろっている():
        # 別プロセスで実行する。tools/spec.py は cwd を基準にしているので、
        # ここから import すると chdir が必要。
        done = subprocess.run(
            [sys.executable, "tools/spec.py", "--check"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        assert done.returncode == 0, done.stderr
