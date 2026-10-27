# Jupyter で使う

leani は Jupyter のカーネルとしても使える。
セルには、端末の leani に入力するのと同じ内容を書く。

## インストール

カーネルは Python 版の leani で動く。[uv](https://docs.astral.sh/uv/) が必要。

```
uv tool install 'leani[jupyter] @ git+https://github.com/watanany/leani'
leani-kernel install
```

`leani --install-kernel` でカーネルを登録した Python 版の leani がある場合は、先に
`uv tool uninstall leani` で消す。Python 版の `leani` コマンド (`~/.local/bin`) が残ると、
端末の `leani` コマンド (`~/.cargo/bin`) とどちらが起動するかが PATH の順で決まる。
登録した「Lean 4 (leani)」カーネルは、別にインストールした JupyterLab からも使える。

## 端末との違い

- **略記は Tab でだけ置き換える**。JupyterLab には space で置き換える仕組みが無いため
- **最初のセルは時間がかかる**。最初のセルでエンジンを起動するので、それまでは定数名の補完も使えない
- **証明モードはセルごとに 1 タクティク**。`:prove` のセルを実行したあと、タクティクを 1 つずつ別のセルに書く
- **`:!` の出力はコマンドが終わってからまとめて表示する**。セルからは入力できないので、`vim` のような対話するコマンドは使えない
