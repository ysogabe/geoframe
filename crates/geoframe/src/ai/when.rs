//! ライブラリの日付の言葉を、LAN のローカル LLM（`llm-vl`）に期間へ読ませる補助（R8b）。
//! 設計は docs/admin-library-browse/design.md の「LLM（R8b、`ai/when.rs`）」。
//!
//! 規則（[`crate::library_when::parse_rule`]）で読めなかった言葉だけがここへ来る。AI 検索（[`super::query`]）と
//! **同じ [`LanClient`]**（宛先は private／loopback のみ・リダイレクトを追わない・要求 8 MiB／応答 1 MiB）と
//! 同じ停止の合図を使い、対話の枠（[`Lane::Interactive`]）を共有する。GPU を守るために:
//!
//! - 送る前に、AI の無効・[`LanClient`] の無し・失敗後の待ち・停止・回数の上限（1 分 20 回）の順で確かめ、
//!   どれかに当たれば送らずに [`Outcome::Unavailable`]。回数は**送る前に予約**する（応答が検証に落ちても数える）。
//! - 期限は 8 秒。枠が使用中（[`HttpError::Busy`]）なら待たずに [`Outcome::Unavailable`]。
//! - 時間切れ・接続の失敗・5xx・429 のあとは 60 秒（`Retry-After` が秒数ならその秒数。上限 300 秒）送らない。
//!
//! 利用者の言葉（`q`）は `user` の `content` にだけ入れ、`serde_json` で本文を組む（文字列の連結で JSON を作らない）。
//! LLM の応答は文字列のまま返さず、形・暦・範囲を検証した日付 2 つ（[`Span`]）だけを外に出す。
//! ログは [`HttpError::as_str`] の固定のコードと経路だけで、言葉・応答の本文・`content` は出さない。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::http::{Deadline, Endpoint, HttpError};
use super::provider::LanClient;
use crate::config::Config;
use crate::library_when::{Date, Span, days_in_month, year_ok};
use crate::stop::StopFlag;

/// 送るモデル（2026-11-02 の試験で使ったもの）。
const MODEL: &str = "qwen3-vl-4b-q4_k_m";
/// 1 回の要求の全体期限。
const DEADLINE: Duration = Duration::from_secs(8);
/// 回数の窓の長さと上限（1 分 20 回）。
const WINDOW: Duration = Duration::from_secs(60);
const PER_WINDOW: usize = 20;
/// 失敗のあとに送らない長さの既定と上限。
const DEFAULT_WAIT: Duration = Duration::from_secs(60);
const MAX_WAIT: Duration = Duration::from_secs(300);
/// 期間の上限（開始日から数えて 20 年。日数ではなく暦で求める。[`span_limit`]）。
const MAX_SPAN_YEARS: i32 = 20;
/// 経路（ログ用の固定の文字）。
const ROUTE: &str = "/llm-vl/v1/chat/completions";

/// LLM に読ませた結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 検証に通った期間。
    Span(Span),
    /// LLM は答えたが、読めなかった・検証に落ちた。
    Unreadable,
    /// LLM が使えない（無効・設定なし・待ち・停止・上限・枠が使用中・通信の失敗）。
    Unavailable,
}

// 利用者の言葉は持たない（回数と待ちの時刻だけ）。
#[derive(Default)]
struct State {
    window: VecDeque<Duration>,
    /// この時刻（`now` の値）までは送らない。
    blocked_until: Option<Duration>,
}

pub struct Runtime {
    enabled: bool,
    lan: Option<LanClient>,
    stop: StopFlag,
    state: Mutex<State>,
    now: Arc<dyn Fn() -> Duration + Send + Sync>,
    limit: Duration,
}

impl Runtime {
    /// `lan` は AI 検索と同じものを clone して渡す（対話の枠を共有するため）。AI が無効（`config.ai.legacy().enabled`
    /// が偽。AI 検索の `query.rs` と同じ）なら、`lan` があっても送らない。
    pub fn new(config: &Config, lan: Option<LanClient>, stop: StopFlag) -> Self {
        let start = Instant::now();
        Self {
            enabled: config.ai.legacy().enabled,
            lan,
            stop,
            state: Mutex::new(State::default()),
            now: Arc::new(move || start.elapsed()),
            limit: DEADLINE,
        }
    }

    /// 試験だけ: 時計と期限を差し替える（60 秒の待ちと 8 秒の期限を実時間で待たないため）。
    #[cfg(test)]
    fn with_clock(mut self, now: Arc<dyn Fn() -> Duration + Send + Sync>, limit: Duration) -> Self {
        self.now = now;
        self.limit = limit;
        self
    }

    /// 規則で読めなかった言葉 `q`（前後の空白を除いたもの）を、基準日 `today` で LLM に読ませる。
    pub async fn read(&self, q: &str, today: Date) -> Outcome {
        if !self.enabled {
            return Outcome::Unavailable;
        }
        let Some(lan) = &self.lan else {
            return Outcome::Unavailable;
        };
        if self.stop.is_stopped() || !self.reserve() {
            return Outcome::Unavailable;
        }
        let body = request_body(q, today).to_string().into_bytes();
        let limit = self.limit;
        let sent = lan
            .http()
            .run_interactive(&self.stop, move |session| {
                session.post_json(Endpoint::VlChat, &body, &Deadline::after(limit))
            })
            .await;
        let reply = match sent.and_then(|reply| reply) {
            Ok(reply) => reply,
            Err(error) => {
                self.failed(error);
                return Outcome::Unavailable;
            }
        };
        if reply.status == 429 || (500..600).contains(&reply.status) {
            tracing::warn!(
                status = reply.status,
                route = ROUTE,
                "日付の言葉の LLM: 一時的な失敗"
            );
            self.block(reply.retry_after_secs.map(u64::from));
            return Outcome::Unavailable;
        }
        if !reply.is_success() {
            // 404（`llm-vl` が通らない）など。待ちは置かない（回数の上限が GPU を守る）
            tracing::warn!(
                status = reply.status,
                route = ROUTE,
                "日付の言葉の LLM: 応答の状態"
            );
            return Outcome::Unavailable;
        }
        match parse_reply(&reply.body, today) {
            Some(span) => Outcome::Span(span),
            None => Outcome::Unreadable,
        }
    }

    /// 失敗後の待ちと回数の上限を、1 つの Mutex の中で確かめ、通れば記録を積む（送る前の予約）。
    fn reserve(&self) -> bool {
        let now = (self.now)();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.blocked_until.is_some_and(|until| now < until) {
            return false;
        }
        while state
            .window
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= WINDOW)
        {
            state.window.pop_front();
        }
        if state.window.len() >= PER_WINDOW {
            return false;
        }
        state.window.push_back(now);
        true
    }

    /// 送れなかった・通信に失敗したとき。枠が使用中・停止は一時的なので待ちを置かない。
    fn failed(&self, error: HttpError) {
        tracing::warn!(
            error = error.as_str(),
            route = ROUTE,
            "日付の言葉の LLM: 失敗"
        );
        if !matches!(
            error,
            HttpError::Busy | HttpError::Stopping | HttpError::RequestTooLarge
        ) {
            self.block(None);
        }
    }

    /// `secs`（`Retry-After`。無ければ既定の 60 秒。上限 300 秒）のあいだ送らない。
    fn block(&self, secs: Option<u64>) {
        let wait = secs
            .filter(|s| *s > 0)
            .map_or(DEFAULT_WAIT, Duration::from_secs)
            .min(MAX_WAIT);
        let until = (self.now)() + wait;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.blocked_until = Some(state.blocked_until.map_or(until, |old| old.max(until)));
    }
}

/// 要求の本文。`q` は `user` の `content` にだけ入る。`system` は固定の指示とサーバーが作った基準日だけ。
fn request_body(q: &str, today: Date) -> Value {
    json!({
        "model": MODEL,
        "temperature": 0,
        "max_tokens": 80,
        "messages": [
            {"role": "system", "content": system_prompt(today)},
            {"role": "user", "content": q},
        ],
    })
}

fn system_prompt(today: Date) -> String {
    format!(
        "あなたは日付の言葉を期間に直す変換器です。利用者の言葉が表す期間を、次の形の JSON だけで答えてください。\
         説明は書かないでください。\n\
         {{\"from\":\"YYYY-MM-DD\",\"to\":\"YYYY-MM-DD\"}}\n\
         from と to は両端を含む日付です。期間が分からないときは {{\"error\":\"unknown\"}} だけを答えてください。\n\
         今日の日付は {} です。\n\
         季節: 春は3月〜5月、夏は6月〜8月、秋は9月〜11月、冬は12月〜翌年2月。\n\
         行事: 正月は1月1日〜1月7日、GW（ゴールデンウィーク）は4月29日〜5月5日、お盆は8月13日〜8月16日。",
        today.format()
    )
}

/// 応答の検証。`choices[0].message.content` の最初の `{` から最後の `}` までを JSON として読み、`from`・`to` を
/// 形（ASCII の数字と `-` だけの 10 文字）→ 暦 → `from <= to` → 年（1900〜基準年+1）→ 20 年以内の順で確かめる。
fn parse_reply(body: &[u8], today: Date) -> Option<Span> {
    let reply: Value = serde_json::from_slice(body).ok()?;
    let content = reply.pointer("/choices/0/message/content")?.as_str()?;
    let (start, end) = (content.find('{')?, content.rfind('}')?);
    if end < start {
        return None;
    }
    let answer: Value = serde_json::from_str(&content[start..=end]).ok()?;
    let from = date_of(answer.get("from")?.as_str()?)?;
    let to = date_of(answer.get("to")?.as_str()?)?;
    let ok = from <= to
        && year_ok(from.year, today)
        && year_ok(to.year, today)
        && to <= span_limit(from);
    ok.then_some(Span { from, to })
}

/// 開始日 `from` から許す最後の日: `from` の 20 年後の同じ月日。そのうるう年の 2/29 を起点にして 20 年後が平年
/// （100 の倍数で 400 の倍数でない年）なら 2/28 にする（その月の末日に丸める）。日数で数えると、20 年の中の
/// うるう日の数（4 回か 5 回）で境目が 1 日ずれる。
fn span_limit(from: Date) -> Date {
    let year = from.year + MAX_SPAN_YEARS;
    let day = from.day.min(days_in_month(year, from.month));
    Date {
        year,
        month: from.month,
        day,
    }
}

/// `YYYY-MM-DD`（ASCII の数字と `-` だけの 10 文字）で、暦に在る日付。
fn date_of(text: &str) -> Option<Date> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    let year = text[0..4].parse().ok()?;
    Date::new(year, text[5..7].parse().ok()?, text[8..10].parse().ok()?)
}

#[cfg(test)]
mod tests;
