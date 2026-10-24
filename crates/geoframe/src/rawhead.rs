//! 受信したバイト列の中の `Origin` ヘッダの行を見て、**値の末尾に空白文字がある**ことを覚える。
//!
//! 現行の `http.server` は `Origin` の値の末尾の空白を残し、`urlparse()` が `netloc` に含めるので
//! `Origin: http://localhost␠`（␠は空白文字）は `Host: localhost` と一致せず 403 になる（タブは
//! `urlparse` が捨てるので通る）。hyper（httparse）は値の両端を取り除いてから渡すため、`post.rs` の
//! 判定からは見えない。そこで [`crate::limits::LimitedIo`] が読んだバイト列を [`OriginTrailingSpace`] に
//! 通して印（[`ConnHints`]）を付け、`post.rs` が値に空白を 1 つ足して判定する（[`crate::csrf`]）。
//!
//! - 行頭が `origin:`（大文字小文字を区別しない）で、値に空白でない文字があり、改行（`\r` を除く）の直前の
//!   文字が空白文字（U+0020）なら印を付ける。値の前の空白は数えない。値の末尾が空白とタブの並びなら、
//!   その中に空白文字が 1 つでもあれば印を付ける（タブだけなら付けない）。
//! - 見るのは**接続に届いた全ての行**。本文の中に `Origin:` で始まる行があれば誤って印が付くが、
//!   JSON の本文にそういう行は無く、印が付いても**断る側**（その接続の以降の `Origin` 付きの POST に
//!   空白を足して判定する）にしか働かない。印は接続ごとで、付いたら消えない（先読みされた次の要求と
//!   取り違えて通してしまうことが無いように）。
//! - 行が複数の読み取りにまたがってもよい（状態を持つ）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;

use crate::limits::LimitedListener;

/// 接続ごとの印。`axum` の `ConnectInfo` として要求の `extensions` に載る。
#[derive(Clone, Debug, Default)]
pub struct ConnHints {
    origin_trailing_space: Arc<AtomicBool>,
}

impl ConnHints {
    /// この接続で、末尾に空白文字がある `Origin` の行を受け取ったことがあるか。
    pub fn origin_trailing_space(&self) -> bool {
        self.origin_trailing_space.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_origin_trailing_space(&self) {
        self.origin_trailing_space.store(true, Ordering::Relaxed);
    }
}

impl Connected<IncomingStream<'_, LimitedListener>> for ConnHints {
    fn connect_info(stream: IncomingStream<'_, LimitedListener>) -> Self {
        stream.io().hints()
    }
}

const NAME: &[u8; 7] = b"origin:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// 行頭から `origin:` のうち何文字まで合っているか。
    Name(usize),
    /// 別のヘッダ（または要求行・本文）の行。改行まで読み飛ばす。
    Skip,
    /// `origin:` の後ろの値を読んでいる。
    Value {
        /// 空白・タブでない文字を見たか。
        content: bool,
        /// 最後の空白・タブの並びに、空白文字（タブでなく）があるか。
        space_in_tail: bool,
    },
}

/// 1 接続ぶんの走査器。
#[derive(Debug)]
pub struct OriginTrailingSpace {
    state: State,
}

impl Default for OriginTrailingSpace {
    fn default() -> Self {
        Self {
            state: State::Name(0),
        }
    }
}

impl OriginTrailingSpace {
    /// 届いたバイト列を読む。印を付けるべき行の終わりを見たら真。
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut found = false;
        for &b in bytes {
            if b == b'\n' {
                if let State::Value {
                    content: true,
                    space_in_tail: true,
                } = self.state
                {
                    found = true;
                }
                self.state = State::Name(0);
                continue;
            }
            self.state = match self.state {
                State::Name(n) if b.to_ascii_lowercase() == NAME[n] => {
                    if n + 1 == NAME.len() {
                        State::Value {
                            content: false,
                            space_in_tail: false,
                        }
                    } else {
                        State::Name(n + 1)
                    }
                }
                State::Name(_) | State::Skip => State::Skip,
                State::Value {
                    content,
                    space_in_tail,
                } => match b {
                    // 行末の `\r` は値に数えない（httparse も取り除く）
                    b'\r' => State::Value {
                        content,
                        space_in_tail,
                    },
                    b' ' => State::Value {
                        content,
                        space_in_tail: true,
                    },
                    b'\t' => State::Value {
                        content,
                        space_in_tail,
                    },
                    _ => State::Value {
                        content: true,
                        space_in_tail: false,
                    },
                },
            };
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> bool {
        let mut scanner = OriginTrailingSpace::default();
        let mut found = false;
        for chunk in chunks {
            found |= scanner.feed(chunk);
        }
        found
    }

    #[test]
    fn a_trailing_space_after_content_is_marked() {
        assert!(scan(&[
            b"POST / HTTP/1.1\r\nOrigin: http://a \r\nHost: a\r\n\r\n"
        ]));
        assert!(scan(&[b"origin:http://a \n"]));
        assert!(scan(&[b"ORIGIN: x\t \t\r\n"]));
        assert!(scan(&[b"Origin: x  \r\n"]));
        // 行頭でなければ見ない
        assert!(!scan(&[b"X-Origin: http://a \r\n"]));
        assert!(!scan(&[b"Origin : http://a \r\n"]));
    }

    #[test]
    fn leading_space_tab_tails_and_empty_values_are_not_marked() {
        assert!(!scan(&[b"Origin: http://a\r\n"]));
        assert!(!scan(&[b"Origin:    http://a\r\n"]));
        // タブは urlparse が捨てる
        assert!(!scan(&[b"Origin: http://a\t\r\n"]));
        assert!(!scan(&[b"Origin: http://a\t\t\r\n"]));
        // 値が空白だけ（現行は空＝無いのと同じ）
        assert!(!scan(&[b"Origin:   \r\n"]));
        assert!(!scan(&[b"Origin:\r\n"]));
        // 空白が最後の文字でなければ（内側の空白）付けない
        assert!(!scan(&[b"Origin: a b\r\n"]));
        assert!(!scan(&[b"Origin: a b\t\r\n"]));
    }

    #[test]
    fn lines_may_be_split_across_reads_at_any_byte() {
        let raw: &[u8] = b"GET / HTTP/1.1\r\nHost: a\r\nOrigin: http://a \r\nX: 1\r\n\r\n";
        for cut in 0..=raw.len() {
            assert!(scan(&[&raw[..cut], &raw[cut..]]), "{cut}");
        }
        // 1 バイトずつ
        let singles: Vec<&[u8]> = raw.chunks(1).collect();
        assert!(scan(&singles));
        let clean: &[u8] = b"GET / HTTP/1.1\r\nOrigin: http://a\r\n\r\n";
        let singles: Vec<&[u8]> = clean.chunks(1).collect();
        assert!(!scan(&singles));
    }

    #[test]
    fn a_marked_line_does_not_leak_into_the_next_line() {
        let mut scanner = OriginTrailingSpace::default();
        assert!(scanner.feed(b"Origin: a \r\n"));
        assert!(!scanner.feed(b"Origin: a\r\n"));
        assert!(!scanner.feed(b"X: a \r\n"));
    }
}
