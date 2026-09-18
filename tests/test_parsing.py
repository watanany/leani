"""入力の読み方を決めている純関数。端末もエンジンも不要なのでミリ秒で終わる。"""

import io
import os
import subprocess
import sys

import pytest
from conftest import ROOT, leani, story
from hypothesis import given
from hypothesis import strategies as st

# ------------------------------------------------- property test の素材

# 括弧が閉じた文字列。中身は Lean の字面に寄せてあるが、閉じ方だけが必要。
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

# sorry の前後に置く行。コメントとして "sorry" を書いたものを混ぜてある。
DECOYS = ["", "def f : True := by", "-- sorry はここにもある", '"sorry"', "  rfl"]

# ヘッダ領域の材料。import・コメント・本体と、import に見えて import でない行。
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
    """「まだ途中」だけを継続と読む。"""

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
    def it_折り返した提案を丸ごと取る():
        # pretty printer が 100 桁前後で折り返す。1 行目だけ取ると
        # `simp only [a, b,` になり、それが完成した証明として出てしまう。
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
    def it_Try_this_と同じ行に書かれた提案も取る():
        found = leani.pure.try_this([{"data": "Try this: exact Nat.le_refl n"}])
        assert found == "exact Nat.le_refl n"

    @story("E2")
    def it_閉じていない提案は読み切れていないと分かる():
        # スクリプトに入れる前の最後の確認。切り落とすと必ず括弧が合わなくなる。
        assert leani.pure.balanced("simp only [aaa, bbb]")
        assert leani.pure.balanced("exact ⟨foo (bar x), rfl⟩")
        assert not leani.pure.balanced("simp only [aaa, bbb,")
        assert not leani.pure.balanced("exact foo)")
        assert not leani.pure.balanced("exact ⟨foo]")

    @story("E2")
    @given(
        a=BALANCED, b=BALANCED, close=st.sampled_from(list(leani.pure.PAIRS.values()))
    )
    def it_閉じたもの同士を繋いでも閉じている(a, b, close):
        # 例では 5 通りしか置けない。閉じた文字列を文法から組んで、繋いでも
        # 閉じたまま・閉じ括弧を 1 つ足せば必ず崩れることを見る。
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


def describe_宣言の名前拾い():
    """自分で通した宣言を補完の候補に足すために使う。"""

    @story("D1")
    def it_修飾子や属性が付いていても名前を取れる():
        cases = {
            "def foo := 1": "foo",
            "theorem bar (n : Nat) : n = n := rfl": "bar",
            "@[simp] private noncomputable def baz : Nat := 0": "baz",
            "structure Point where\n  x : Nat": "Point",
        }
        for src, want in cases.items():
            assert leani.queries.DECL_NAME.findall(src) == [want], f"取れない: {src!r}"

    @story("D1")
    def it_宣言でないものは拾わない():
        assert leani.queries.DECL_NAME.findall("#eval 1 + 1") == []


def describe_補完をまとめて取る単位():
    """名前空間があればそこまで、無ければ先頭 2 文字。"""

    @story("D1")
    def it_名前空間で切る():
        assert leani.repl.Repl._chunk("Nat.suc") == "Nat."
        assert leani.repl.Repl._chunk("MeasureTheory.integral_") == "MeasureTheory."

    @story("D1")
    def it_名前空間が無ければ二文字():
        # mathlib では 1 文字だと `C` で 7.5 万件になるので広げすぎない。
        assert leani.repl.Repl._chunk("Contin") == "Co"


def describe_完結したかの読み分け():
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
    def it_深く進めた側をユーザの意図とみなす():
        probe = {
            "cmd": {"ok": False, "err": "<input>:1:2: unexpected token"},
            "term": {"ok": False, "err": "<input>:1:7: unexpected token"},
        }
        assert leani.pure.classify(probe) == (leani.types.ERR, leani.types.TERM)

    @story("C1")
    def it_判定できなければ_Lean_に本当のエラーを出させる():
        # 自前の推測でエラーを作らない。command として投げる。
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


def describe_エラー位置の枠():
    """メッセージの位置を、送ったソースの座標からその行の中に収める。"""

    @story("A2")
    def it_eval_で包んだ分だけ位置を戻す():
        m = {"pos": {"line": 2, "column": 6}, "endPos": {"line": 2, "column": 9}}
        # #eval で 1 行 2 桁ずらして送っているので、元のソースでは 1 行 4 桁。
        assert leani.pure.span(m, ["foo bar"], line_off=1, col_off=2) == (1, 4, 3)

    @story("A2")
    def it_行の外なら枠を出さない():
        m = {"pos": {"line": 9, "column": 0}}
        assert leani.pure.span(m, ["foo"], 0, 0) is None

    @story("A2")
    def it_幅は行の長さに収める():
        m = {"pos": {"line": 1, "column": 1}, "endPos": {"line": 1, "column": 99}}
        assert leani.pure.span(m, ["abc"], 0, 0) == (1, 1, 2)


def describe_レスポンスの切り出し():
    """空行で区切られた JSON を、揃った分だけ取る。"""

    @story("F1")
    def it_揃うまでは何も返さない():
        assert leani.pure.first_response('{"env": 1}') is None

    @story("F1")
    def it_空行まで来たら読む():
        assert leani.pure.first_response('{"env": 1}\n\n') == {"env": 1}

    @story("F1")
    def it_JSON_の中の空行では切らない():
        buf = '{"messages": [{"data": "a\\n\\nb"}]}\n\n'
        assert leani.pure.first_response(buf)["messages"][0]["data"] == "a\n\nb"


def describe_起動前の下ごしらえ():
    """import を足したり落としたりするところ。"""

    @story("G2")
    def it_init_ファイルの_import_行は落とす():
        # 既にある環境に重ねるので、import は書けない。
        assert leani.pure.strip_imports("import Foo\ndef x := 1") == "def x := 1"

    @story("G2")
    def it_import_を持つファイルには足さない():
        assert leani.pure.has_import("\nimport Mathlib\ndef f := 1")
        assert not leani.pure.has_import("def f := 1  -- import ではない")

    @story("G1")
    def it_lake_env_のキャッシュを読む():
        env = leani.pure.parse_env_lines("LEAN_PATH=/a:/b\nLD_LIBRARY_PATH=\n")
        assert env == {"LEAN_PATH": "/a:/b", "LD_LIBRARY_PATH": ""}

    @story("G3", "G4")
    def it_toolchain_が動いたらキャッシュを取り直す(tmp_path):
        # LEAN_PATH は core の olean も指す。前のバージョンのキャッシュを使い回すと
        # repl は起動するのに import が丸ごと落ちて、原因が見えなくなる。
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
    def it_読めないバージョンは_None():
        for tag in ("v4.33", "nightly-2026-01-01", "4.33.0"):
            assert leani.pure.version_key(tag) is None, f"読めてしまう: {tag!r}"

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
    def it_パスから外へ出るバージョン名は受け付けない():
        # build_engine はこの場所を rmtree してから置き直す。`..` を通すと
        # 関係ないディレクトリを消してしまう。
        for tc in ("leanprover/lean4:..", "..", "."):
            with pytest.raises(leani.types.EngineError):
                leani.pure.engine_dir(None, tc)


def describe_設定の値の型():
    """TOML は何でも書ける。型が違う分は traceback ではなく ConfigError。"""

    @story("G4")
    def it_import_を文字列で書いたらエラーにする():
        # 黙って受けると 1 文字ずつの import になり、どれも解決できないので
        # ヘッダが丸ごと捨てられる (import Lean ごと消える)。
        cfg = {"default": "m", "env": {"m": {"imports": "Mathlib"}}}
        with pytest.raises(leani.types.ConfigError, match="配列"):
            leani.config.resolve(cfg=cfg)

    @story("G4")
    def it_文字列で書くべき所が別の型ならエラーにする():
        for key, value in (("project", 3), ("engine", True), ("prompt", 5)):
            cfg = {"default": "m", "env": {"m": {key: value}}}
            with pytest.raises(leani.types.ConfigError, match=key):
                leani.config.resolve(cfg=cfg)

    @story("G4")
    def it_表で書くべき所が別の型ならエラーにする():
        with pytest.raises(leani.types.ConfigError, match="env は表"):
            leani.config.resolve(cfg={"default": "m", "env": 3})
        with pytest.raises(leani.types.ConfigError, match=r"env\.m は表"):
            leani.config.resolve(cfg={"default": "m", "env": {"m": "nope"}})

    @story("G4")
    def it_default_が文字列でなければエラーにする():
        with pytest.raises(leani.types.ConfigError, match="default"):
            leani.config.resolve(cfg={"default": 3, "env": {"m": {}}})

    @story("G4", "G5")
    def it_書き方が正しければそのまま通る():
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

    エンジンをビルドしたバージョンと合わないと olean が読めない。
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
    def it_明示したエンジンのバージョンに倒す(tmp_path):
        # 手動のエンジンは leani がビルドし直さないので、そのバージョンに
        # 合わせるしかない。
        # elan の既定バージョンに落ちると 4.34 の olean を 4.33 で読むことになる。
        cfg = leani.config.EnvConfig.make(
            "t", engine=_toolchain_dir(tmp_path, "eng", "leanprover/lean4:v4.33.0")
        )
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:v4.33.0"

    @story("G3")
    def it_どちらも無ければ_elan_の既定(mocker):
        mocker.patch.object(
            leani.boot, "local_toolchain", return_value="leanprover/lean4:v4.9.0"
        )
        cfg = leani.config.EnvConfig.make("t")
        assert leani.boot.guess_toolchain(cfg) == "leanprover/lean4:v4.9.0"


def describe_手で用意する手順():
    """自動で用意できなかったときに出す。そのままなぞって同じものになる形。"""

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
    """clone と build のふりをする。clone 先を seen に残す。"""

    def run(cmd, cwd=None):
        if cmd[0] == "git":
            seen.append(cmd[-1])
            os.makedirs(cmd[-1])
        else:
            os.makedirs(f"{cwd}/.lake/build/bin")
            open(f"{cwd}/.lake/build/bin/repl", "w").close()

    return run


def describe_エンジンを用意するときの安全側():
    """消してよいのは leani が掘ったところだけ。同時に 2 つ走ってもよい。"""

    @story("G3")
    def it_明示されたエンジンは作り直さない(tmp_path, mocker):
        # build_engine は置き場を作り直すので、人が指したところに向けてはならない。
        build = mocker.patch.object(leani.boot, "build_engine")
        eng = _toolchain_dir(tmp_path, "eng", "leanprover/lean4:v4.33.0")
        with pytest.raises(leani.types.EngineError, match="未ビルド"):
            leani.boot.ensure_engine(eng, "leanprover/lean4:v4.33.0")

        assert not build.called
        assert os.path.isfile(f"{eng}/lean-toolchain")

    @story("G3", "G4")
    def it_明示されたエンジンにバージョンが無ければ起動を中止する(tmp_path, mocker):
        # バージョンを合わせる先がこれしかない。黙って elan の既定に落ちると
        # olean が読めない。
        mocker.patch.object(leani.config.shutil, "which", return_value="/usr/bin/x")
        (tmp_path / "eng/.lake/build/bin").mkdir(parents=True)
        (tmp_path / "eng/.lake/build/bin/repl").touch()
        cfg = leani.config.EnvConfig.make("t", engine=str(tmp_path / "eng"))
        assert "lean-toolchain" in (leani.config.problem(cfg) or "")

    @story("G3")
    def it_途中の置き場はプロセスごとに分かれる(tmp_path, mocker):
        # leani を 2 つ同時に起動したとき、同じ .tmp を使うと片方が起動できない。
        seen = []
        mocker.patch.object(leani.boot, "run_setup", side_effect=_fake_setup(seen))
        path = str(tmp_path / "engine" / "v4.33.0")
        leani.boot.build_engine(path, "leanprover/lean4:v4.33.0", "v4.33.0")

        assert str(os.getpid()) in seen[0], seen
        assert os.path.isfile(f"{path}/.lake/build/bin/repl")
        assert not os.path.exists(seen[0]), "途中の置き場が残っている"

    @story("G3")
    def it_待っている間に別が置いたなら消さない(tmp_path, mocker):
        # 消すと、そのエンジンで動いている別の leani の足元が抜ける。
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


def describe_sorry_の埋め戻し():
    """
    repl の pos / endPos で切ってスクリプトに差し替える。桁は codepoint 単位で
    Python の添字と揃っている (日本語コメントを挟んで実測した)。
    """

    @story("E3")
    def it_日本語を挟んでも位置で切れる():
        src = "theorem a : True := by /- あいう -/ sorry"
        got = leani.pure.splice_sorry(src, mark(src), "trivial")
        assert got == "theorem a : True := by /- あいう -/ trivial", got

    @story("E3")
    def it_行頭に寄っている_sorry_の桁を動かさない():
        # 空白を詰めると桁が浅くなり、内側の by ブロックから外れる。
        src = "example : True := by\n  have h : True := by\n    sorry\n  exact h"
        got = leani.pure.splice_sorry(src, mark(src), "trivial")
        assert got == (
            "example : True := by\n  have h : True := by\n    trivial\n  exact h"
        ), got

    @story("E3")
    def it_複数行のスクリプトは_sorry_の桁に揃える():
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
    def it_行の途中なら_by_の下にぶら下げる():
        src = "theorem t (n : Nat) : n + 0 = n := by sorry"
        got = leani.pure.splice_sorry(src, mark(src), "induction n with\n| zero => rfl")
        assert got == (
            "theorem t (n : Nat) : n + 0 = n := by\n  induction n with\n  | zero => rfl"
        ), got

    @story("E3")
    def it_組の中の_sorry_に空白を足さない():
        src = "example : True ∧ True := ⟨by sorry, by trivial⟩"
        got = leani.pure.splice_sorry(src, mark(src), "trivial")
        assert got == "example : True ∧ True := ⟨by trivial, by trivial⟩", got

    @story("E3")
    def it_位置が_sorry_を指していなければ諦める():
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
    def it_行と桁を文字位置に直せる(lines, n, col):
        # splice_sorry が切る位置はこれで決まる。1 桁ずれると sorry を跨いで切る。
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
        # 前後にコメントとして "sorry" を混ぜる。数えて当てていると、そちらを
        # 切ってしまう。位置で切っている限り、前後は一字も動かない。
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
        # 行の途中に置いた sorry だけは、手前の空白を落として by の下に下げる。
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
        # 書けば repl はヘッダを丸ごと捨てて起動するので、書き出したファイルは
        # :l でも lean でも通らなくなる。落とす側も doc コメントを壊す。
        src = "/-\nimport Nope.NotAModule\n-/\n-- import Also.Not\nimport Std\n"
        assert leani.pure.import_lines(src) == ["Std"]
        assert "import Nope.NotAModule" in leani.pure.strip_imports(src)
        assert "import Std" not in leani.pure.strip_imports(src)

    @story("B3")
    def it_宣言のあとの_import_は数えない():
        # Lean も先頭以外の import を認めない。数えると :save のヘッダが増える。
        assert leani.pure.import_lines("def a := 1\nimport Std\n") == []

    @story("B4")
    def it_入れ子のコメントを閉じ切る():
        src = "/- /- import In.Nest -/ import Still.In -/\nimport Real\n"
        assert leani.pure.import_lines(src) == ["Real"]

    @story("B3", "B4")
    @given(lines=st.lists(st.sampled_from(HEADERS), max_size=12))
    def it_落とすのは本物の_import_行だけ(lines):
        src = "\n".join(lines)
        mods, at = leani.pure.scan_header(src)
        stripped = leani.pure.strip_imports(src)

        assert len(mods) == len(at)
        # コメントの中の import を拾っていない。拾えば :save したファイルが壊れる。
        assert all(lines[n].split()[:1] == ["import"] for n in at)
        # 落とした跡から新しい import は生えない。ヘッダの終わりは動かないので、
        # 1 回で落としきる。
        assert leani.pure.import_lines(stripped) == []


def describe_略記の展開():
    """
    `\name` を Lean の記号にする。確定は space に任せて、打鍵ごとには見ない。
    """

    @story("C5")
    def it_名前を記号にする():
        assert leani.abbrev.expand_abbrev("a \\to") == ("→", 3)
        assert leani.abbrev.expand_abbrev("x\\dot") == ("·", 4)

    @story("C5")
    def it_短い名前に食われない():
        # `\a` も `\all` も `\alpha` も表にある。打鍵ごとに確定すると `\a` で
        # 決まってしまい、長い方を打てなくなる。
        assert leani.abbrev.expand_abbrev("\\a") == ("α", 2)
        assert leani.abbrev.expand_abbrev("\\all") == ("∀", 4)
        assert leani.abbrev.expand_abbrev("\\alpha") == ("α", 6)

    @story("C5")
    def it_滅多に打たない略記も変換できる():
        # 表は本家の全件。よく打つ分だけに絞っていた頃は変換できなかった。
        assert leani.abbrev.expand_abbrev("\\frown") == ("⌢", 6)
        assert leani.abbrev.expand_abbrev("\\Gangia") == ("Ϫ", 7)

    @story("C5")
    def it_表に無い名前は変えない():
        assert leani.abbrev.expand_abbrev("\\nosuch") is None
        assert leani.abbrev.expand_abbrev("1 + 1") is None

    @story("C5")
    def it_記号が続く形は変えない():
        # `\to)` まで丸ごと表を引く。`"\t"` のような文字列を ▸ に化けさせない。
        assert leani.abbrev.expand_abbrev("(\\to)") is None
        assert leani.abbrev.expand_abbrev('"\\t"') is None

    @story("C5")
    @given(
        key=st.sampled_from(sorted(leani.abbrev.ABBREV)),
        head=st.text(alphabet=st.characters(exclude_characters="\\"), max_size=8),
    )
    def it_表のどの略記も変換できる(key, head):
        # 例に書けるのは数件。1829 件すべてが同じ規則で変換できることはここで見る。
        # 打つ手前に何が書いてあっても、直前の `\\` から後ろだけを鍵にする。
        want = (leani.abbrev.ABBREV[key], len(key) + 1)
        assert leani.abbrev.expand_abbrev(head + "\\" + key) == want


def describe_定理検索():
    """
    loogle に聞いた結果を出す形にする。外に聞く所だけが副作用で、整形は純粋。
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
    def it_名前と型と居場所を出す():
        out = leani.pure.loogle_text({"count": 2, "hits": HITS}).split("\n")
        assert out[0] == "2 件"
        # `type` は先頭に空白が付いて来るので、`name : type` に組み直す。
        assert out[1] == "List.map : (f : α → β) : List α → List β"
        # どの module にあるかを添える。手元の環境に無い名前も出てくるため。
        assert out[2] == "  Init.Prelude"
        assert out[3].startswith("List.mapTR : ")

    @story("D4")
    def it_絞ったときは全体の件数を添える():
        # loogle は 200 件で切って返すので、count は hits より多いことがある。
        got = {"count": 360, "hits": HITS}
        assert (
            leani.pure.loogle_text(got, keep=1).split("\n")[0] == "360 件 (先頭 1 件)"
        )
        assert leani.pure.loogle_text({"count": 2, "hits": HITS}).count("\n") == 4

    @story("D4")
    def it_長い行は幅で切る():
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
    def it_エラーは訳さずに候補を添える():
        # loogle の文言は Lean のパーサのもの。日本語にすると元の位置情報が消える。
        got = {"error": "unknown identifier 'Nope'", "suggestions": ['"Nope"']}
        out = leani.pure.loogle_text(got).split("\n")
        assert out == ["loogle: unknown identifier 'Nope'", 'もしかして: "Nope"']

    @story("D4")
    def it_記号を含むクエリも壊さずに送る(mocker):
        # `?a + ?b` の `+` をそのまま URL に置くと、向こうでは空白として読まれる。
        opened = mocker.patch(
            "urllib.request.urlopen",
            return_value=io.BytesIO(b'{"count": 0, "hits": []}'),
        )
        leani.search.loogle("?a + ?b")
        assert opened.call_args[0][0].endswith("?q=%3Fa%20%2B%20%3Fb")

    @story("D4")
    def it_届かなければ_SearchError(mocker):
        # ネットワークの事情で REPL を終了させない。呼ぶ側が受けて 1 行報告する。
        mocker.patch("urllib.request.urlopen", side_effect=OSError("名前が引けない"))
        with pytest.raises(leani.types.SearchError, match="名前が引けない"):
            leani.search.loogle("Nat")


def describe_性質一覧():
    """
    SPEC.md はテスト名から生成する (tools/spec.py)。手で書き足す場所を
    作らない代わりに、生成し忘れをここで落とす。
    """

    @story("X1")
    def it_SPEC_md_がテストと揃っている():
        # 別プロセスで走らせる。tools/spec.py は cwd を基準にしているので、
        # ここから import すると chdir が必要。
        done = subprocess.run(
            [sys.executable, "tools/spec.py", "--check"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        assert done.returncode == 0, done.stderr
