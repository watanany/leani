//! 確定した入力を 1 件として保存する履歴 (副作用)。
//!
//! 行編集の crate は 1 行読むたびにその行を履歴に追加しようとするが、`def fib` の
//! 4 行が 4 件になると、呼び出すのに Ctrl-P を 4 回押す必要がある。どこまでを 1 件と
//! するかは `Repl` が判断する (submit / discard) ので、行ごとの追加は無視して、
//! `record` による追加だけを受け付ける。
//!
//! 履歴ファイルには追記だけをする。読めない形式のファイル (readline や libedit の
//! 履歴) があっても、読めない行を無視するだけで、ファイルは上書きしない。形式の
//! 読み書きは `leani_core::history` にある。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use leani_core::history::{history_entry, history_strings};

pub struct BlockHistory {
    path: PathBuf,
}

impl BlockHistory {
    pub fn new(path: impl AsRef<Path>) -> BlockHistory {
        BlockHistory {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 行ごとの追加。何もしない (行編集の crate が呼ぶ分を捨てる)。
    pub fn append_string(&self, _line: &str) {}

    /// 1 件として追記する。書けなくても (ディレクトリが無い、権限が無い) 履歴は
    /// 残らないだけなので、エラーは報告しない。
    pub fn record(&self, src: &str) {
        let stamp = chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S%.6f")
            .to_string();
        let entry = history_entry(&stamp, src);
        if let Ok(mut f) = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
        {
            let _ = f.write_all(entry.as_bytes());
        }
    }

    /// 記録した順に件を返す。ファイルが無ければ空。
    pub fn load(&self) -> Vec<String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => history_strings(&String::from_utf8_lossy(&bytes)),
            Err(_) => Vec::new(),
        }
    }
}
