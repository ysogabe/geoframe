//! ライブラリの日付の言葉（「去年の夏」「2024年10月〜2月」など）を期間に直す規則（R8a）。
//! docs/admin-library-browse/design.md の「`GET /api/admin/library/when`」の「規則」の節。
//!
//! このファイルは**純粋な関数だけ**を持つ。基準日（今日）は引数で受け取り（サーバーの現地日付を
//! 取るのは HTTP の処理の側）、入出力・ログ・時計は使わない。入力の文字列は利用者の言葉なので、
//! どこにも出さない（ログ・エラー文に含めない）。どんな文字列でもパニックせず、`None` か `Some` を返す。
//!
//! 暦の関数（うるう年・月の日数・曜日・日付の足し引き）は自前の小さなもの。chrono は
//! dev-dependency にしか無く、新しい crate は足さない。
//!
//! 読む順番（設計書どおり）:
//! 1. 前処理のあと、文字列全体を 1 つの期間として読む（`2024-08`・`2024-08-15`・`2024/8` はここ。
//!    範囲の `-` と取り違えないため、`-` は範囲の区切りに使わない）。
//! 2. 読めなければ `〜`・`から`（と末尾の `まで`）で 2 つに分けて範囲として読む。
//! 3. どちらでも読めなければ `None`（呼び出し側が LLM へ回す）。

/// 入力（前後の空白を除いたもの、正規化のあと）の文字数の上限。`photos` の `q` の上限
/// （`NAME_MAX_CHARS`）と同じ値。
pub const INPUT_MAX_CHARS: usize = 64;

/// 年として読む下限。これより前の年は読めない。上限は基準年+1。
const MIN_YEAR: i32 = 1900;

/// 暦の日付。`year` は西暦、`month` は 1〜12、`day` は 1〜その月の日数。
/// 並びの順は年・月・日なので、導出した `Ord` が日付の前後と一致する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// 読んだ期間。両端を含む（`from` ≤ `to`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: Date,
    pub to: Date,
}

/// うるう年か（400 で割れる、または 4 で割れて 100 で割れない）。
pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// その月の日数。月が 1〜12 の外なら 0（呼び出し側の検査で必ず落ちる値）。
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// `year` 年 `month` 月に `delta` か月を足した年月。
fn add_months(year: i32, month: u32, delta: i64) -> (i32, u32) {
    let index = i64::from(year) * 12 + i64::from(month) - 1 + delta;
    let y = index.div_euclid(12);
    let m = index.rem_euclid(12);
    // 年は i32 に収まる範囲の入力しか来ない（年の桁数と基準日の検査で絞っている）が、
    // 万一でもパニックはしない（`as` は丸めるだけ）。後の検査で年の範囲外として落ちる。
    (y as i32, m as u32 + 1)
}

impl Date {
    /// 暦に在る日付だけを作る。
    pub fn new(year: i32, month: u32, day: u32) -> Option<Date> {
        if day >= 1 && day <= days_in_month(year, month) {
            Some(Date { year, month, day })
        } else {
            None
        }
    }

    /// 1970-01-01 からの日数（Howard Hinnant の days_from_civil）。
    pub fn to_days(self) -> i64 {
        let m = i64::from(self.month);
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12; // 3月 = 0
        let doy = (153 * mp + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// [`Date::to_days`] の逆（civil_from_days）。
    pub fn from_days(days: i64) -> Date {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Date {
            year: year as i32,
            month: month as u32,
            day: day as u32,
        }
    }

    /// `n` 日後（負なら前）。
    pub fn add_days(self, n: i64) -> Date {
        Date::from_days(self.to_days() + n)
    }

    /// 曜日。月曜 = 0 … 日曜 = 6（週の頭が月曜の「今週」「先週」で使う）。
    /// 1970-01-01 は木曜（3）。
    pub fn weekday_mon0(self) -> u32 {
        (self.to_days() + 3).rem_euclid(7) as u32
    }

    /// `YYYY-MM-DD`。
    pub fn format(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// 月・日が暦に正しいか（基準日の検査用）。
    fn is_valid(self) -> bool {
        Date::new(self.year, self.month, self.day).is_some()
    }
}

impl Span {
    /// `/photos`・`/points`・`/places` に渡せる 16 桁の形。`from` は `YYYY-MM-DD 00:00`、
    /// `to` は `YYYY-MM-DD 23:59`。
    pub fn query_range(&self) -> (String, String) {
        (
            format!("{} 00:00", self.from.format()),
            format!("{} 23:59", self.to.format()),
        )
    }
}

/// 全角の数字・英字・記号を半角にし、空白（全角も）を除き、英字は小文字にする。
///
/// - U+FF01〜U+FF5E は ASCII に 0xFEE0 を足した並びなので、まとめて引く（`０-９`・`Ａ-Ｚ`・`／`・`－`・`～`）。
/// - 波ダッシュ `〜`（U+301C）は `~` に、ハイフンの類（`−`・`‐` など）は `-` にする。
/// - 長音 `ー` は「ゴールデンウィーク」の中にも在るので、数字に挟まれたときだけ `-` に直す
///   （`2024ー08`）。
pub fn normalize(text: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(text.len().min(256));
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        let c = match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{301C}' | '\u{223C}' => '~',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            other => other,
        };
        out.push(c.to_ascii_lowercase());
    }
    for i in 1..out.len().saturating_sub(1) {
        if out[i] == 'ー' && out[i - 1].is_ascii_digit() && out[i + 1].is_ascii_digit() {
            out[i] = '-';
        }
    }
    out.into_iter().collect()
}

/// 入力の長さの検査。前後の空白を除いて 1〜[`INPUT_MAX_CHARS`] 文字で、正規化のあとにも
/// [`INPUT_MAX_CHARS`] 文字を超えないこと。外れたら偽（HTTP の側が 400 にする）。
/// `text` はパーセント解除のあとの文字列。[`parse_rule`] の前に呼ぶ。
pub fn input_len_ok(text: &str) -> bool {
    let n = text.trim().chars().count();
    (1..=INPUT_MAX_CHARS).contains(&n) && normalize(text).chars().count() <= INPUT_MAX_CHARS
}

/// 日付の言葉を期間に直す。読めなければ `None`（LLM へ回す）。`today` は基準日（現地日付）。
/// 長さの検査（[`input_len_ok`]）は呼ぶ側の仕事だが、ここでも上限を超える入力は読まずに `None` にする。
pub fn parse_rule(text: &str, today: Date) -> Option<Span> {
    if !today.is_valid() {
        return None;
    }
    let norm: Vec<char> = normalize(text).chars().collect();
    if norm.is_empty() || norm.len() > INPUT_MAX_CHARS {
        return None;
    }
    let span = match parse_piece(&norm, today) {
        Some(span) => span,
        None => parse_range(&norm, today)?,
    };
    // 完成した期間の両端の年が読める範囲（1900〜基準年+1）にあること。部品ごとの検査では、
    // 「2020年代」の終わりや「2027年冬」の翌年に出る終わりを見逃す。
    (year_ok(span.from.year, today) && year_ok(span.to.year, today)).then_some(span)
}

// ---- 部品 ----

/// 年の書かれていない部品（月だけ・季節・行事）の始まりと終わり。年が付けば「その年に始まる」もの。
/// `end_day` が 0 なら終わりの月の末日（2 月末はうるう年で変わる）。
#[derive(Clone, Copy)]
struct Yearless {
    start_month: u32,
    start_day: u32,
    end_month: u32,
    end_day: u32,
}

impl Yearless {
    /// `year` 年に始まるもの。終わりの月が始まりの月より前なら翌年に終わる（冬・年末年始）。
    fn in_year(self, year: i32) -> Option<Span> {
        let from = Date::new(year, self.start_month, self.start_day)?;
        let end_year = if self.end_month < self.start_month {
            year + 1
        } else {
            year
        };
        let end_day = if self.end_day == 0 {
            days_in_month(end_year, self.end_month)
        } else {
            self.end_day
        };
        let to = Date::new(end_year, self.end_month, end_day)?;
        Some(Span { from, to })
    }

    /// 年なしの規則: 始まりの日が基準日以前で、いちばん新しいもの。始まりは必ずその年の中なので、
    /// 基準年か前の年のどちらか。
    fn latest(self, today: Date) -> Option<Span> {
        let this_year = self.in_year(today.year)?;
        if this_year.from <= today {
            return year_ok(today.year, today).then_some(this_year);
        }
        let last_year = today.year - 1;
        if year_ok(last_year, today) {
            self.in_year(last_year)
        } else {
            None
        }
    }
}

/// 年が読める範囲（1900〜基準年+1）か。
fn year_ok(year: i32, today: Date) -> bool {
    (MIN_YEAR..=today.year.saturating_add(1)).contains(&year)
}

/// 年を付けずに読む部品（月だけ・季節・行事）を、基準日で年を決めずに構造だけ読む。
/// 範囲の右側で左の年を引き継ぐときに使う。
fn parse_yearless(s: &[char]) -> Option<Yearless> {
    if let Some(yl) = yearless_word(s) {
        return Some(yl);
    }
    let (m, i) = num(s, 0, 2)?;
    if !is_word(s, i, "月") {
        return None;
    }
    let month = m as u32;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some(Yearless {
        start_month: month,
        start_day: 1,
        end_month: month,
        end_day: 0,
    })
}

/// 年なしの語（季節・行事）。`gw` は正規化で小文字になっている。
fn yearless_word(word: &[char]) -> Option<Yearless> {
    let w: String = word.iter().collect();
    let (sm, sd, em, ed) = match w.as_str() {
        "春" => (3, 1, 5, 31),
        "夏" => (6, 1, 8, 31),
        "秋" => (9, 1, 11, 30),
        "冬" => (12, 1, 2, 0),
        "正月" => (1, 1, 1, 7),
        "gw" | "ゴールデンウィーク" => (4, 29, 5, 5),
        "お盆" => (8, 13, 8, 16),
        "年末年始" => (12, 28, 1, 3),
        _ => return None,
    };
    Some(Yearless {
        start_month: sm,
        start_day: sd,
        end_month: em,
        end_day: ed,
    })
}

/// `s[i..]` から ASCII の数字を最大 `max` 桁まで読む。1 桁も無ければ `None`。値と次の位置を返す。
fn num(s: &[char], i: usize, max: usize) -> Option<(i64, usize)> {
    let mut j = i;
    let mut v: i64 = 0;
    while j < s.len() && j - i < max && s[j].is_ascii_digit() {
        v = v * 10 + i64::from(s[j].to_digit(10)?);
        j += 1;
    }
    (j > i).then_some((v, j))
}

/// `s[i..]` が `word` で始まるか。
fn starts_with(s: &[char], i: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(k, c)| s.get(i + k) == Some(&c))
}

/// `s[i..]` がちょうど `word` か。
fn is_word(s: &[char], i: usize, word: &str) -> bool {
    starts_with(s, i, word) && s.len() == i + word.chars().count()
}

fn month_span(year: i32, month: u32) -> Option<Span> {
    Some(Span {
        from: Date::new(year, month, 1)?,
        to: Date::new(year, month, days_in_month(year, month))?,
    })
}

fn year_span(year: i32) -> Option<Span> {
    Some(Span {
        from: Date::new(year, 1, 1)?,
        to: Date::new(year, 12, 31)?,
    })
}

/// 文字列全体を 1 つの期間として読む。
fn parse_piece(s: &[char], today: Date) -> Option<Span> {
    if s.is_empty() {
        return None;
    }
    // 今週: 基準日の週の月曜〜基準日。先週: 前の週の月〜日。最近: 基準日の 30 日前〜基準日（両端を含む 31 日）。
    if is_word(s, 0, "今週") {
        let monday = today.add_days(-i64::from(today.weekday_mon0()));
        return Some(Span {
            from: monday,
            to: today,
        });
    }
    if is_word(s, 0, "先週") {
        let monday = today.add_days(-i64::from(today.weekday_mon0()));
        return Some(Span {
            from: monday.add_days(-7),
            to: monday.add_days(-1),
        });
    }
    if is_word(s, 0, "最近") {
        return Some(Span {
            from: today.add_days(-30),
            to: today,
        });
    }
    if is_word(s, 0, "今月") {
        return relative_month(today, 0);
    }
    if is_word(s, 0, "先月") {
        return relative_month(today, 1);
    }
    if let Some(piece) = parse_months_ago(s, today) {
        return Some(piece);
    }
    if let Some(piece) = parse_numeric(s, today) {
        return Some(piece);
    }
    // 年なしの部品（季節・行事・月だけ）は年なしの規則で決める。
    if let Some(yl) = parse_yearless(s) {
        return yl.latest(today);
    }
    parse_with_year_prefix(s, today)
}

/// `N か月前`（`か`・`ヶ`・`ケ`・`ヵ`・`カ` のどれでも）。N は 1 以上。
fn parse_months_ago(s: &[char], today: Date) -> Option<Span> {
    let (n, i) = num(s, 0, 4)?;
    if n < 1 || !matches!(s.get(i), Some('か' | 'ヶ' | 'ケ' | 'ヵ' | 'カ')) {
        return None;
    }
    if !is_word(s, i + 1, "月前") {
        return None;
    }
    relative_month(today, n)
}

/// 基準日の月から `ago` か月前の月。
fn relative_month(today: Date, ago: i64) -> Option<Span> {
    let (y, m) = add_months(today.year, today.month, -ago);
    if !year_ok(y, today) {
        return None;
    }
    month_span(y, m)
}

/// 数字の並びの形: `2025`・`2010年代`・`2024-08`・`2024/8`・`2024-08-15`（区切りは `/` か `-`、
/// 月と日の 2 つとも同じ区切り）。
fn parse_numeric(s: &[char], today: Date) -> Option<Span> {
    let (y, i) = num(s, 0, 4)?;
    if i != 4 {
        return None;
    }
    let year = y as i32;
    if !year_ok(year, today) {
        return None;
    }
    if i == s.len() {
        return year_span(year);
    }
    if is_word(s, i, "年代") {
        if year % 10 != 0 {
            return None;
        }
        return Some(Span {
            from: Date::new(year, 1, 1)?,
            to: Date::new(year + 9, 12, 31)?,
        });
    }
    let sep = *s.get(i)?;
    if sep != '/' && sep != '-' {
        return None;
    }
    let (m, j) = num(s, i + 1, 2)?;
    let month = m as u32;
    if j == s.len() {
        return month_span(year, month);
    }
    if s[j] != sep {
        return None;
    }
    let (d, k) = num(s, j + 1, 2)?;
    if k != s.len() {
        return None;
    }
    let day = Date::new(year, month, d as u32)?;
    Some(Span { from: day, to: day })
}

/// 年の前置き（`2024年`・`今年`・`去年`・`昨年`・`おととし`・`一昨年`・`N年前`、あとに `の` が付いてもよい）と、
/// そのあとの部分（なし＝その年・`8月`・`8月15日`・季節・行事）。
fn parse_with_year_prefix(s: &[char], today: Date) -> Option<Span> {
    let (year, mut i) = year_prefix(s, today)?;
    if !year_ok(year, today) {
        return None;
    }
    if s.get(i) == Some(&'の') {
        i += 1;
    }
    let rest = &s[i..];
    if rest.is_empty() {
        return year_span(year);
    }
    if let Some(yl) = yearless_word(rest) {
        return yl.in_year(year);
    }
    let (m, j) = num(rest, 0, 2)?;
    if !starts_with(rest, j, "月") {
        return None;
    }
    let month = m as u32;
    let after = j + 1;
    if after == rest.len() {
        return month_span(year, month);
    }
    let (d, k) = num(rest, after, 2)?;
    if !is_word(rest, k, "日") {
        return None;
    }
    let day = Date::new(year, month, d as u32)?;
    Some(Span { from: day, to: day })
}

/// 年の前置きを読み、年と読み進めた位置（`の` の前）を返す。
fn year_prefix(s: &[char], today: Date) -> Option<(i32, usize)> {
    for (word, ago) in [
        ("今年", 0),
        ("去年", 1),
        ("昨年", 1),
        ("おととし", 2),
        ("一昨年", 2),
    ] {
        if starts_with(s, 0, word) {
            return Some((today.year - ago, word.chars().count()));
        }
    }
    let (n, i) = num(s, 0, 4)?;
    if starts_with(s, i, "年前") {
        if n < 1 {
            return None;
        }
        let year = i64::from(today.year) - n;
        return Some((i32::try_from(year).ok()?, i + 2));
    }
    if i != 4 {
        return None;
    }
    // 「2024年末年始」の `年` は「年末年始」の頭なので食わない。
    if starts_with(s, i, "年末年始") && s.len() == i + 4 {
        return Some((n as i32, i));
    }
    if s.get(i) == Some(&'年') {
        return Some((n as i32, i + 1));
    }
    None
}

// ---- 範囲 ----

/// `〜`・`から`（末尾の `まで` は除く）で 2 つに分けて範囲として読む。区切りの位置を左から順に試す。
fn parse_range(s: &[char], today: Date) -> Option<Span> {
    let mut seps: Vec<(usize, usize)> = Vec::new();
    for (i, c) in s.iter().enumerate() {
        if *c == '~' {
            seps.push((i, 1));
        }
    }
    for i in 0..s.len().saturating_sub(1) {
        if s[i] == 'か' && s[i + 1] == 'ら' {
            seps.push((i, 2));
        }
    }
    for (at, len) in seps {
        let left = &s[..at];
        let mut right = &s[at + len..];
        if right.len() >= 2 && is_word(right, right.len() - 2, "まで") {
            right = &right[..right.len() - 2];
        }
        if let Some(span) = join_range(left, right, today) {
            return Some(span);
        }
    }
    None
}

/// 左の始まり〜右の終わり。
///
/// - 右に年が書かれていない（月だけ・季節・行事）ときだけ左の年を引き継ぎ、それで右が左より前になるなら
///   右を翌年にする。
/// - 右に年が書かれている（`今年2月`・`2023年`）ときは引き継がない。右が左より前なら読めない。
fn join_range(left: &[char], right: &[char], today: Date) -> Option<Span> {
    let l = parse_piece(left, today)?;
    // 右が年なしの部品なら、基準日では決めず構造だけ読み、左の年を引き継いでから年を検査する。
    let right_span = match parse_yearless(right) {
        Some(yl) => {
            let year = l.from.year;
            let mut span = yl.in_year(year)?;
            if span.from < l.from {
                span = yl.in_year(year + 1)?;
            }
            span
        }
        None => parse_piece(right, today)?,
    };
    if right_span.from < l.from {
        return None;
    }
    Some(Span {
        from: l.from,
        to: right_span.to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date::new(y, m, day).expect("試験の日付は暦に在る")
    }

    fn today_a() -> Date {
        d(2026, 11, 2)
    }

    /// 基準日 `today`（"YYYY-MM-DD"）で `input` を読み、期待の `from`・`to`（"YYYY-MM-DD"）と比べる。
    fn check_on(today: &str, input: &str, expected: Option<(&str, &str)>) {
        let t: Vec<i32> = today.split('-').map(|x| x.parse().unwrap()).collect();
        let today = d(t[0], t[1] as u32, t[2] as u32);
        let got = parse_rule(input, today).map(|s| (s.from.format(), s.to.format()));
        let want = expected.map(|(a, b)| (a.to_string(), b.to_string()));
        assert_eq!(got, want, "基準日 {today:?} の入力 {input:?}");
    }

    fn ok(input: &str, from: &str, to: &str) {
        check_on("2026-11-02", input, Some((from, to)));
    }

    fn unreadable(input: &str) {
        check_on("2026-11-02", input, None);
    }

    #[test]
    fn table_for_2026_11_02() {
        ok("2024年8月", "2024-08-01", "2024-08-31");
        ok("2024-08", "2024-08-01", "2024-08-31");
        ok("2024-08-15", "2024-08-15", "2024-08-15");
        ok("去年の夏", "2025-06-01", "2025-08-31");
        ok("先月", "2026-10-01", "2026-10-31");
        ok("3年前の正月", "2023-01-01", "2023-01-07");
        ok("2023年の春〜秋", "2023-03-01", "2023-11-30");
        ok("去年のGW", "2025-04-29", "2025-05-05");
        ok("今年の冬", "2026-12-01", "2027-02-28");
        ok("冬", "2025-12-01", "2026-02-28");
        ok("2年前のお盆", "2024-08-13", "2024-08-16");
        ok("おととしの12月", "2024-12-01", "2024-12-31");
        ok("最近", "2026-10-03", "2026-11-02");
        ok("2025年", "2025-01-01", "2025-12-31");
        ok("今週", "2026-11-02", "2026-11-02");
        ok("先週", "2026-10-26", "2026-11-01");
        ok("昨年10月から今年2月まで", "2025-10-01", "2026-02-28");
        ok("2024年10月〜2月", "2024-10-01", "2025-02-28");
        ok("8月", "2026-08-01", "2026-08-31");
        ok("12月", "2025-12-01", "2025-12-31");
        ok("2010年代", "2010-01-01", "2019-12-31");
        unreadable("2023年〜2022年");
        unreadable("2025年13月");
        unreadable("1800年");
    }

    #[test]
    fn table_for_other_base_dates() {
        check_on("2027-01-02", "冬", Some(("2026-12-01", "2027-02-28")));
        check_on("2027-01-02", "年末年始", Some(("2026-12-28", "2027-01-03")));
        check_on("2027-01-02", "正月", Some(("2027-01-01", "2027-01-07")));
        check_on("2028-03-10", "冬", Some(("2027-12-01", "2028-02-29")));
        check_on("2026-03-01", "春", Some(("2026-03-01", "2026-05-31")));
        check_on("2026-02-28", "春", Some(("2025-03-01", "2025-05-31")));
    }

    #[test]
    fn unreadable_examples() {
        unreadable("娘が生まれた頃");
        unreadable("2023年〜2022年");
        unreadable("2025年13月");
        unreadable("1800年");
        unreadable("2025年2月30日");
        unreadable("2023年2月29日");
        unreadable("");
        unreadable("〜");
        unreadable("から");
        unreadable("2月から");
        unreadable("2024年8月-10月");
        unreadable("8月15日");
    }

    #[test]
    fn leap_day_is_valid_only_in_leap_years() {
        ok("2024年2月29日", "2024-02-29", "2024-02-29");
        ok("2024年2月", "2024-02-01", "2024-02-29");
        ok("2025年2月", "2025-02-01", "2025-02-28");
        ok("2000-02-29", "2000-02-29", "2000-02-29");
        unreadable("1900-02-29");
    }

    #[test]
    fn year_bounds() {
        ok("1900年", "1900-01-01", "1900-12-31");
        unreadable("1899年");
        // 基準年+1 まで読める
        ok("2027年", "2027-01-01", "2027-12-31");
        unreadable("2028年");
        unreadable("2028-01");
        unreadable("2100年");
        unreadable("200年");
        unreadable("20250年");
        unreadable("200000年前");
        unreadable("127年前");
    }

    #[test]
    fn completed_span_years_are_checked() {
        // 基準年+1（2027）を超えて終わる期間は読めない
        unreadable("2020年代");
        unreadable("2027年冬");
        unreadable("2027年10月〜冬");
        // 境界の内側は読める
        ok("2027年", "2027-01-01", "2027-12-31");
        ok("2026年の冬", "2026-12-01", "2027-02-28");
        ok("今年の冬", "2026-12-01", "2027-02-28");
        ok("2010年代", "2010-01-01", "2019-12-31");
        // 下限側
        unreadable("1890年代");
    }

    #[test]
    fn range_right_side_without_year_inherits_left_year() {
        // 右の「2月」を基準日だけで決めると 1899 年になって落ちる
        check_on(
            "1900-01-01",
            "1900年1月〜2月",
            Some(("1900-01-01", "1900-02-28")),
        );
        // 翌年に回っても基準年+1 までなら読める。その先は読めない
        check_on(
            "1900-01-01",
            "1900年12月〜2月",
            Some(("1900-12-01", "1901-02-28")),
        );
        check_on("1900-01-01", "1901年12月〜2月", None);
        check_on(
            "2026-11-02",
            "2026年10月〜冬",
            Some(("2026-10-01", "2027-02-28")),
        );
    }

    #[test]
    fn added_interpretations_are_fixed() {
        ok("8月〜12月", "2026-08-01", "2026-12-31");
        unreadable("12-3");
        unreadable("8/15");
        unreadable("2024-8/15");
        ok("2024-8-15", "2024-08-15", "2024-08-15");
        ok("2024年8月〜10月まで", "2024-08-01", "2024-10-31");
    }

    #[test]
    fn fullwidth_and_spaces() {
        ok("２０２４年８月", "2024-08-01", "2024-08-31");
        ok("２０２４－０８", "2024-08-01", "2024-08-31");
        ok("２０２４／８", "2024-08-01", "2024-08-31");
        ok("2024ー08ー15", "2024-08-15", "2024-08-15");
        ok("2024年 8月", "2024-08-01", "2024-08-31");
        ok(" 去年の　夏 ", "2025-06-01", "2025-08-31");
        ok("２０２３年の春～秋", "2023-03-01", "2023-11-30");
        ok("２０２３年の春〜秋", "2023-03-01", "2023-11-30");
        ok("ｇｗ", "2026-04-29", "2026-05-05");
        ok("Ｇ　Ｗ", "2026-04-29", "2026-05-05");
        // 長音は数字に挟まれたときだけ `-` になる
        ok("ゴールデンウィーク", "2026-04-29", "2026-05-05");
    }

    #[test]
    fn relative_months_and_years() {
        ok("今月", "2026-11-01", "2026-11-30");
        ok("1か月前", "2026-10-01", "2026-10-31");
        ok("3ヶ月前", "2026-08-01", "2026-08-31");
        ok("11ヶ月前", "2025-12-01", "2025-12-31");
        ok("２カ月前", "2026-09-01", "2026-09-30");
        ok("10ケ月前", "2026-01-01", "2026-01-31");
        ok("今年", "2026-01-01", "2026-12-31");
        ok("去年", "2025-01-01", "2025-12-31");
        ok("昨年", "2025-01-01", "2025-12-31");
        ok("おととし", "2024-01-01", "2024-12-31");
        ok("一昨年", "2024-01-01", "2024-12-31");
        ok("3年前", "2023-01-01", "2023-12-31");
        ok("3年前の8月", "2023-08-01", "2023-08-31");
        ok("去年12月", "2025-12-01", "2025-12-31");
        ok("去年の8月15日", "2025-08-15", "2025-08-15");
        ok("今年の夏", "2026-06-01", "2026-08-31");
        ok("今年の年末年始", "2026-12-28", "2027-01-03");
        ok("2024年末年始", "2024-12-28", "2025-01-03");
        ok("2024年年末年始", "2024-12-28", "2025-01-03");
        ok("2024年の正月", "2024-01-01", "2024-01-07");
        unreadable("0か月前");
        unreadable("0年前");
        unreadable("来月");
    }

    #[test]
    fn yearless_rule_picks_latest_start_not_after_base_date() {
        // 基準日ちょうどに始まるものはその年
        check_on("2026-08-13", "お盆", Some(("2026-08-13", "2026-08-16")));
        // 始まりの前日なら前の年
        check_on("2026-08-12", "お盆", Some(("2025-08-13", "2025-08-16")));
        check_on("2026-12-31", "年末年始", Some(("2026-12-28", "2027-01-03")));
        check_on("2026-12-27", "年末年始", Some(("2025-12-28", "2026-01-03")));
        check_on("2026-01-01", "正月", Some(("2026-01-01", "2026-01-07")));
        check_on("2026-11-02", "秋", Some(("2026-09-01", "2026-11-30")));
        check_on("2026-11-02", "2月", Some(("2026-02-01", "2026-02-28")));
        check_on("2028-03-10", "2月", Some(("2028-02-01", "2028-02-29")));
    }

    #[test]
    fn ranges() {
        ok("2023年〜2024年", "2023-01-01", "2024-12-31");
        ok("2023年から2024年まで", "2023-01-01", "2024-12-31");
        ok("2024年8月〜2024年10月", "2024-08-01", "2024-10-31");
        ok("2024年8月〜10月", "2024-08-01", "2024-10-31");
        ok("2024年8月から10月", "2024-08-01", "2024-10-31");
        ok("2024-08-01〜2024-08-20", "2024-08-01", "2024-08-20");
        ok("2024-08-01から2024-08-20まで", "2024-08-01", "2024-08-20");
        // 右に年が無ければ左の年を引き継ぎ、前になるなら翌年
        ok("2024年12月〜2月", "2024-12-01", "2025-02-28");
        ok("2024年12月〜冬", "2024-12-01", "2025-02-28");
        ok("2023年冬〜春", "2023-12-01", "2024-05-31");
        // 左も年なしなら年なしの規則の年を引き継ぐ
        ok("8月〜10月", "2026-08-01", "2026-10-31");
        ok("12月〜2月", "2025-12-01", "2026-02-28");
        // 右に年があれば引き継がず、逆転なら読めない
        unreadable("2024年10月〜2023年2月");
        unreadable("2024年10月〜今年2月から");
        ok("去年10月〜今年2月", "2025-10-01", "2026-02-28");
        unreadable("今年2月〜去年10月");
        unreadable("2024年8月から2023年");
        // 右が基準年+1 を越えるなら読めない
        unreadable("2027年10月〜2月");
        ok("先週〜今週", "2026-10-26", "2026-11-02");
        unreadable("2024年〜");
        unreadable("〜2024年");
        unreadable("2024年〜2025年〜2026年");
    }

    #[test]
    fn query_range_has_sixteen_char_form() {
        let span = parse_rule("2024年8月", today_a()).unwrap();
        assert_eq!(
            span.query_range(),
            (
                "2024-08-01 00:00".to_string(),
                "2024-08-31 23:59".to_string()
            )
        );
        let (from, to) = span.query_range();
        assert_eq!(from.len(), 16);
        assert_eq!(to.len(), 16);
    }

    #[test]
    fn input_length_check() {
        assert!(input_len_ok("去年の夏"));
        assert!(input_len_ok("  去年の夏  "));
        assert!(!input_len_ok(""));
        assert!(!input_len_ok("   "));
        assert!(!input_len_ok("　"));
        let ok64 = "あ".repeat(64);
        assert!(input_len_ok(&ok64));
        assert!(input_len_ok(&format!(" {ok64} ")));
        assert!(!input_len_ok(&"あ".repeat(65)));
        // 前後の空白を除いた長さで数える（中の空白は数える）
        assert!(!input_len_ok(&format!("あ {}", "あ".repeat(63))));
        // parse_rule 自身も、長すぎる入力は読まない
        let long = format!("2024年8月{}", " ".repeat(10) + &"あ".repeat(70));
        assert_eq!(parse_rule(&long, today_a()), None);
        assert_eq!(
            parse_rule(&format!("{}2024年", "0".repeat(70)), today_a()),
            None
        );
    }

    #[test]
    fn calendar_helpers() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2025));
        assert!(!is_leap_year(2100));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2025, 4), 30);
        assert_eq!(days_in_month(2025, 12), 31);
        assert_eq!(days_in_month(2025, 0), 0);
        assert_eq!(days_in_month(2025, 13), 0);
        assert!(Date::new(2025, 2, 29).is_none());
        assert!(Date::new(2025, 0, 1).is_none());
        assert!(Date::new(2025, 1, 0).is_none());
        // 月曜 = 0
        assert_eq!(d(2026, 11, 2).weekday_mon0(), 0);
        assert_eq!(d(2026, 11, 1).weekday_mon0(), 6);
        assert_eq!(d(1970, 1, 1).weekday_mon0(), 3); // 木曜
        assert_eq!(d(2000, 1, 1).weekday_mon0(), 5); // 土曜
        assert_eq!(d(2024, 2, 29).weekday_mon0(), 3); // 木曜
        assert_eq!(d(1900, 1, 1).weekday_mon0(), 0); // 月曜
        // 日付の足し引き
        assert_eq!(d(2024, 2, 28).add_days(1), d(2024, 2, 29));
        assert_eq!(d(2025, 2, 28).add_days(1), d(2025, 3, 1));
        assert_eq!(d(2026, 1, 1).add_days(-1), d(2025, 12, 31));
        assert_eq!(d(2026, 11, 2).add_days(-30), d(2026, 10, 3));
        assert_eq!(d(1970, 1, 1).to_days(), 0);
        assert_eq!(Date::from_days(0), d(1970, 1, 1));
        for days in [-100_000, -1, 0, 1, 11_016, 20_000, 60_000] {
            assert_eq!(Date::from_days(days).to_days(), days);
        }
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 11, -11), (2025, 12));
        assert_eq!(add_months(2026, 11, -12), (2025, 11));
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
    }

    #[test]
    fn never_panics() {
        let samples = [
            "",
            "年",
            "年前",
            "月",
            "日",
            "の",
            "まで",
            "から",
            "〜〜",
            "~",
            "999999999999999999999999年前",
            "0000年",
            "9999年9999月9999日",
            "2024-",
            "2024-08-",
            "2024/08/15/",
            "年末年始年末年始",
            "ー",
            "1ー",
            "ー1",
            "1ー1",
            "😀😀😀",
            "\u{0}\u{1}\u{FF5E}\u{FF01}",
            "今年の",
            "去年の月",
            "去年の8月の日",
            "か月前",
            "1か月",
            "1年",
            "2024年年",
            "２０２４年９９月",
        ];
        for t in samples {
            let _ = input_len_ok(t);
            let _ = parse_rule(t, today_a());
            let _ = parse_rule(t, d(1900, 1, 1));
            let _ = parse_rule(t, d(9999, 12, 31));
        }
        // 暦に無い基準日は読まない
        let bad = Date {
            year: 2026,
            month: 13,
            day: 40,
        };
        assert_eq!(parse_rule("去年", bad), None);
        // 長い数字の並び・全ての 1 文字
        for c in (0u32..0x3200).filter_map(char::from_u32) {
            let _ = parse_rule(&c.to_string(), today_a());
        }
    }
}
