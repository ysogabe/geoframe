//! 本番の photo_judge handler。画像準備・ファイル観測・通信は writer / DB tx の外で行い、
//! 保存は `jobs::complete` の短い tx 内の CAS（[`persist`]）だけ。
use super::repository::{candidate_current, hidden_ok, input_current, lookup_in, table_present};
use super::*;
use crate::{
    ai::{
        cloud::{CloudClient, CloudClients},
        groups::runtime::Prepared,
        image_input::ImageProfile,
        jobs::{Claimed, ErrorCode, Recheck},
        location_clue::read_image_source,
        settings::TaskKind,
        worker::{Handler, Persist, RunCtx, RunError},
    },
    config::Config,
    datadir::DataDir,
    imaging,
};
use rusqlite::{Connection, OptionalExtension};
use std::sync::{Arc, Mutex, PoisonError};

/// 1 回の reconcile が積む最大件数。実際に送る数は周期の上限・日の上限・失敗の上限が別に抑える。
const MAX_QUEUE: usize = 32;

/// 用途の許可・クラウドの接続が揃ったときだけ作る。無ければ handler は無く、ジョブは claim されない。
pub(in crate::ai) fn handler(
    config: &Config,
    data: DataDir,
    cloud: Option<&CloudClients>,
) -> Option<Arc<dyn Handler>> {
    if !super::enabled(config) {
        return None;
    }
    let gate = config.ai.gate(TaskKind::PhotoJudge, true).ok()?;
    let client = cloud?.client(TaskKind::PhotoJudge)?;
    Some(Arc::new(PhotoJudge {
        config: config.clone(),
        data,
        model: gate.settings.model.clone(),
        include_hidden: gate.settings.include_hidden,
        cloud: client,
        work: Mutex::new(Vec::new()),
    }))
}

struct PhotoJudge {
    config: Config,
    data: DataDir,
    model: String,
    include_hidden: bool,
    cloud: CloudClient,
    work: Mutex<Vec<Candidate>>,
}

fn db_error(_: rusqlite::Error) -> RunError {
    RunError::Attempt(ErrorCode::Internal)
}
fn scene_error(_: crate::ai::groups::Error) -> RunError {
    RunError::Attempt(ErrorCode::Internal)
}

impl PhotoJudge {
    /// 現世代の置換候補のうち、まだ回答が無く（再利用できず）送ってよいもの。大きい場面から最大 [`MAX_QUEUE`]。
    fn scan(&self, read: &Connection) -> rusqlite::Result<Vec<Candidate>> {
        let prepared =
            Prepared::read(read, &self.config).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = read.unchecked_transaction()?;
        let loaded = prepared
            .load_in(&tx)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        // 履歴の表が無いとき（Unverified と同じ扱い）は何も送らない。存在の確認は 1 回だけ。
        if !table_present(&tx)? {
            tx.commit()?;
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for group in loaded.groups() {
            let Some(candidate) = Candidate::from_group(group).filter(Candidate::sendable) else {
                continue;
            };
            if hidden_ok(&tx, &candidate.sent_ids(), self.include_hidden)?
                && matches!(
                    lookup_in(&tx, &candidate.input_key(&self.model))?,
                    Lookup::Absent
                )
            {
                found.push((
                    group.members().len(),
                    group.result_id().to_string(),
                    candidate,
                ));
            }
        }
        tx.commit()?;
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        Ok(found.into_iter().take(MAX_QUEUE).map(|f| f.2).collect())
    }
    fn work(&self, key: &JobKey) -> Option<Candidate> {
        self.work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|c| c.key(&self.model) == *key)
            .cloned()
    }
    /// 送る直前の現在性: 現世代の Disjoint 候補が同じ shortlist で在り、hidden が許可内で、回答が未保存。
    fn check(&self, conn: &Connection, wanted: &Candidate) -> Result<(), RunError> {
        let prepared = Prepared::read(conn, &self.config).map_err(scene_error)?;
        let tx = conn.unchecked_transaction().map_err(db_error)?;
        let current = candidate_current(&tx, &prepared, wanted).map_err(db_error)?;
        let current = current.is_some_and(|c| c.sendable())
            && hidden_ok(&tx, &wanted.sent_ids(), self.include_hidden).map_err(db_error)?
            && matches!(
                lookup(&tx, &wanted.input_key(&self.model)).map_err(db_error)?,
                Lookup::Absent
            );
        tx.commit().map_err(db_error)?;
        if current {
            Ok(())
        } else {
            Err(RunError::InputChanged)
        }
    }
    /// 応答後の現在性: 送った入力（写真の ID と世代）が今も同じで、hidden が許可内で、回答が未保存。
    /// 場面の状態は見ない（有償回答は、場面が一時的に Waiting や Partial になっても履歴に残す）。
    fn check_input(&self, conn: &Connection, wanted: &Candidate) -> Result<(), RunError> {
        let tx = conn.unchecked_transaction().map_err(db_error)?;
        let current = input_current(&tx, wanted, self.include_hidden).map_err(db_error)?
            && matches!(
                lookup(&tx, &wanted.input_key(&self.model)).map_err(db_error)?,
                Lookup::Absent
            );
        tx.commit().map_err(db_error)?;
        if current {
            Ok(())
        } else {
            Err(RunError::InputChanged)
        }
    }
    /// 写真の現在の表示ファイルと、分析済みの内容 SHA。品質の分析が合っていない写真は送らない。
    fn source(&self, conn: &Connection, id: i64) -> Result<(String, String), RunError> {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT p.display,f.content_sha256 FROM photos p
                 JOIN photo_quality_features f ON f.photo_id=p.id AND f.display IS p.display
                  AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                 WHERE p.id=?1 AND COALESCE(f.error,'')='' AND f.content_sha256 IS NOT NULL
                  AND p.display IS NOT NULL AND p.display!=''",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        row.ok_or(RunError::InputChanged)
    }
    /// 実ファイルの SHA が分析済みと同じことを確かめ、RGB から新しい JPEG を作る（EXIF/GPS は入らない）。
    fn images(&self, ctx: &RunCtx<'_>, candidate: &Candidate) -> Result<Vec<SendImage>, RunError> {
        let mut out = Vec::new();
        for (id, _) in &candidate.sent {
            if ctx.is_stopping() {
                return Err(RunError::Stopping);
            }
            let (display, sha_expected) = self.source(ctx.read(), *id)?;
            let bytes = read_image_source(&self.data, &display)?;
            if sha(&bytes) != sha_expected {
                return Err(RunError::InputChanged);
            }
            let rgb = imaging::prepare_embedding_rgb(&bytes, !display.starts_with("derivatives/"))
                .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
            drop(bytes);
            let image = PreparedImage::encode_jpeg_rgb(
                &rgb.pixels,
                rgb.width as u16,
                rgb.height as u16,
                82,
                ImageProfile::Embedding,
            )
            .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
            out.push(SendImage {
                photo_id: *id,
                source_sha256: sha_expected,
                image,
            });
        }
        Ok(out)
    }
    /// 応答後、送った写真のファイルが送信時と同じか（DB の世代だけでは分からない差し替えを防ぐ）。
    fn unchanged(&self, ctx: &RunCtx<'_>, plan: &Plan) -> Result<(), RunError> {
        for sent in &plan.manifest.sent {
            let (display, sha_expected) = self.source(ctx.read(), sent.photo_id)?;
            let bytes = read_image_source(&self.data, &display)?;
            if sha_expected != sent.source_sha256 || sha(&bytes) != sent.source_sha256 {
                return Err(RunError::InputChanged);
            }
        }
        Ok(())
    }
}

impl Handler for PhotoJudge {
    fn kind(&self) -> JobKind {
        JobKind::PhotoJudge
    }
    fn reconcile(&self, read: &Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        let work = self.scan(read)?;
        let keys = work.iter().map(|c| c.key(&self.model)).collect();
        *self.work.lock().unwrap_or_else(PoisonError::into_inner) = work;
        Ok(keys)
    }
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
        let candidate = self.work(&job.key).ok_or(RunError::InputChanged)?;
        self.check(ctx.read(), &candidate)?;
        let images = self.images(ctx, &candidate)?;
        let plan = Plan::new(candidate, &self.model, images).map_err(|e| {
            RunError::Permanent(match e {
                DomainError::TooLarge => ErrorCode::InputTooLarge,
                _ => ErrorCode::InputInvalid,
            })
        })?;
        if plan.key() != job.key {
            return Err(RunError::InputChanged);
        }
        // 設定・許可・停止・予算（毎 POST）は run_checked が送信ごとに確かめる。
        let reply = self.cloud.run_checked(ctx, plan.request(), |_| {
            self.check(ctx.read(), plan.candidate())
        })?;
        let verdict = Verdict::parse(reply.json_text(), plan.sent_ids().len())
            .map_err(|_| RunError::Attempt(ErrorCode::BadResponse))?;
        // 有償の応答を受けた後。追加の外部 I/O（通信）はせず、送った入力の現在性だけを確かめて保存へ渡す。
        self.check_input(ctx.read(), plan.candidate())?;
        self.unchanged(ctx, &plan)?;
        let (claimed, now, hidden) = (job.clone(), ctx.now(), self.include_hidden);
        Ok(Box::new(move |tx| {
            #[cfg(test)]
            let started = std::time::Instant::now();
            let out = persist(tx, &claimed, &plan, &verdict, hidden, now);
            #[cfg(test)]
            PERSIST_TIMES
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(started.elapsed());
            out
        }))
    }
    fn prepare_retry(&self) -> rusqlite::Result<()> {
        let read = crate::db::open_read_only(&self.data.root().join(crate::config::DB_FILE_NAME))
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        *self.work.lock().unwrap_or_else(PoisonError::into_inner) = self.scan(&read)?;
        Ok(())
    }
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck> {
        if key.processor_version == processor_version(&self.model)
            && matches!(lookup(conn, &key.input_hash)?, Lookup::Reusable(_))
        {
            return Ok(Recheck::Satisfied);
        }
        Ok(if self.work(key).is_some() {
            Recheck::Requeue
        } else {
            Recheck::Obsolete
        })
    }
}

/// 試験用: writer の中の `persist` の所要時間（計測の試験が読む）。
#[cfg(test)]
static PERSIST_TIMES: Mutex<Vec<std::time::Duration>> = Mutex::new(Vec::new());

#[cfg(test)]
mod tests;
