//! `recover` コマンド（design.md 4.2 の復旧、6.2）。実行枠を取ってから動く（取れなければ「実行中」で終了コード 4）。
//!
//! - `recover`: 未完了（`running` のまま残った）ジョブと、解除していない `blocked` のジョブの記録を、Drive の今の状態と
//!   照らし合わせて表示するだけ（[`crate::publish::Recovery::explain`]。Drive も、記録も変えない）。完了済みでも、手作業の
//!   片付けが要る知らせ（`notes`。残った `incoming` など）がある記録は、知らせだけを表示する（Drive は照会しない。解除の対象では
//!   ない）。読めない記録・`jobs/` を一覧できないことも表示する（読めない記録は `--resolve` では解除できない。中身を確かめて、そのファイルを手で退かす）。
//! - `recover --resolve <job_id>`: 対象が `blocked`（未解除）のときだけ、解除の日時を記録に書く。Drive には触れない。
//!   `replaced/`・`incoming/` に残ったもののパスと ID（記録から）を表示するだけで、片付けはオーナーが Drive の画面で行う。
//!   `blocked` でない（未完了・終わった・解除済み）、記録が読めない・無いときは断る。
//!
//! 終了コード: 0（表示できた・解除した）、1（解除を断った・記録を書けない）、2（ジョブ ID の形が違う・状態ディレクトリを
//! 用意できない）、4（実行中で実行枠が取れない）。

use std::io::Write;

use crate::app::EXIT_USAGE;
use crate::drive::Layout;
use crate::folder::FolderName;
use crate::jobid::JobId;
use crate::jobs::{JobRecord, JobStore, ReadOutcome, Status};
use crate::lock::{self, Slot};
use crate::publish::Recovery;
use crate::remote::Remote;
use crate::settings::Settings;
use crate::sys;

pub const EXIT_OK: u8 = 0;
pub const EXIT_REFUSED: u8 = 1;
pub const EXIT_BUSY: u8 = 4;

pub fn recover(
    settings: &Settings,
    resolve: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let store = match JobStore::open(&settings.state_dir) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                err,
                "geoframe-export: 状態ディレクトリを用意できません: {e}"
            );
            return EXIT_USAGE;
        }
    };
    let _guard = match lock::try_acquire(&settings.state_dir) {
        Ok(Slot::Got(g)) => g,
        Ok(Slot::Busy) => {
            let _ = writeln!(
                err,
                "geoframe-export: 書き出しが実行中です（実行枠が取れません）。終わってからもう一度実行してください"
            );
            return EXIT_BUSY;
        }
        Err(e) => {
            let _ = writeln!(err, "geoframe-export: 実行枠を取れません: {e}");
            return EXIT_REFUSED;
        }
    };
    match resolve {
        Some(id) => resolve_job(settings, &store, id, out, err),
        None => show(settings, &store, out),
    }
}

fn layout_of(settings: &Settings, rec: &JobRecord) -> Option<Layout> {
    let folder = FolderName::for_period(&rec.request.period).ok()?;
    Layout::new(
        settings,
        rec.request.destination,
        folder,
        rec.job_id.clone(),
    )
    .ok()
}

fn show(settings: &Settings, store: &JobStore, out: &mut dyn Write) -> u8 {
    let scan = store.scan();
    let mut any = false;
    if scan.dir_unreadable {
        any = true;
        let _ = writeln!(
            out,
            "ジョブの記録のディレクトリ jobs/ を一覧できません。権限と持ち主を確かめてください。"
        );
    }
    for name in &scan.unreadable {
        any = true;
        let _ = writeln!(
            out,
            "読めない記録: jobs/{name}。中身を確かめて、そのファイルを手で退かしてください（recover --resolve では解除できません）。\
             退かすまで、新しい書き出しは断られます。"
        );
    }
    let remote = match Remote::new(settings) {
        Ok(r) => r,
        Err(_) => {
            let _ = writeln!(
                out,
                "Drive の資格情報を読めません。--drive-credentials のファイルを確かめてください。"
            );
            return EXIT_REFUSED;
        }
    };
    // 古いほうから
    for rec in scan.records.iter().rev() {
        if !(rec.is_unfinished() || rec.is_blocking()) {
            // 完了済みでも、手作業の片付けが要る知らせ（`notes`。残った `incoming`・公開の記録に書けなかった・旧フォルダが
            // 残った、など）が残っている記録は、知らせとして出す（解除の対象ではない。Drive にも触れない）
            if !rec.notes.is_empty() {
                any = true;
                let _ = writeln!(
                    out,
                    "ジョブ {}（{}、{}〜{}、{}、開始 {}）: 終わっていますが、手作業の片付けが要る知らせが残っています（解除の対象ではありません）",
                    rec.job_id,
                    rec.status.as_str(),
                    rec.request.period.from(),
                    rec.request.period.to(),
                    rec.request.destination,
                    rec.started_at
                );
                for note in &rec.notes {
                    let _ = writeln!(out, "  知らせ: {note}");
                }
            }
            continue;
        }
        any = true;
        let _ = writeln!(
            out,
            "ジョブ {}（{}、{}〜{}、{}、開始 {}）",
            rec.job_id,
            rec.status.as_str(),
            rec.request.period.from(),
            rec.request.period.to(),
            rec.request.destination,
            rec.started_at
        );
        let _ = writeln!(
            out,
            "  記録した段階: {}",
            rec.progress
                .as_ref()
                .map_or_else(|| "なし".to_owned(), |p| p.stage.to_string())
        );
        if let Some(b) = &rec.blocked {
            let _ = writeln!(out, "  blocked の理由: {}", b.reason);
            if !b.description.is_empty() {
                let _ = writeln!(out, "  止めたときの説明: {}", b.description);
            }
            let _ = writeln!(
                out,
                "  Drive を確かめたら、geoframe-export recover --resolve {} で解除してください。",
                rec.job_id
            );
        }
        match &rec.progress {
            None if rec.is_unfinished() => {
                let _ = writeln!(
                    out,
                    "  Drive に触れる前に止まった記録です。次の書き出しの前（またはサービスの起動時）に interrupted にします。"
                );
            }
            None => {}
            Some(progress) => match layout_of(settings, rec) {
                None => {
                    let _ = writeln!(
                        out,
                        "  記録から Drive のパスを作れません。記録を確かめてください。"
                    );
                }
                Some(layout) => {
                    let recovery = Recovery {
                        remote: &remote,
                        layout: &layout,
                        published: &crate::published::PublishedStore::in_state_dir(
                            &settings.state_dir,
                        ),
                        expected: None,
                        timing: crate::publish::Timing::default(),
                    };
                    match recovery.explain(progress) {
                        Ok((_, _, text)) => {
                            let _ = writeln!(out, "  Drive の今の状態と、表のとおりの動き: {text}");
                            if rec.is_blocking() {
                                let _ = writeln!(
                                    out,
                                    "  （blocked のジョブは自動では動かしません。上の動きは参考で、Drive を確かめて解除してください）"
                                );
                            }
                        }
                        Err(e) => {
                            let _ = writeln!(
                                out,
                                "  Drive の今の状態を照会できませんでした（{}）。",
                                e.code
                            );
                        }
                    }
                }
            },
        }
        for note in &rec.notes {
            let _ = writeln!(out, "  知らせ: {note}");
        }
    }
    if !any {
        let _ = writeln!(
            out,
            "未完了のジョブも、blocked のジョブも、片付けの知らせが残っているジョブもありません。"
        );
    }
    EXIT_OK
}

fn resolve_job(
    settings: &Settings,
    store: &JobStore,
    raw: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let Some(id) = JobId::parse(raw) else {
        let _ = writeln!(
            err,
            "geoframe-export: ジョブ ID の形が違います（小文字の 16 進 32 文字）"
        );
        return EXIT_USAGE;
    };
    let mut rec = match store.read(&id) {
        ReadOutcome::Found(rec) => *rec,
        ReadOutcome::NotFound => {
            let _ = writeln!(err, "geoframe-export: そのジョブの記録がありません");
            return EXIT_REFUSED;
        }
        ReadOutcome::Unreadable => {
            let _ = writeln!(
                err,
                "geoframe-export: その記録（jobs/{id}.json）は読めないので、解除できません。\
                 中身を確かめて、そのファイルを手で退かしてください"
            );
            return EXIT_REFUSED;
        }
    };
    if rec.status != Status::Blocked {
        let _ = writeln!(
            err,
            "geoframe-export: そのジョブは blocked ではない（status: {}）ので、解除できません",
            rec.status.as_str()
        );
        return EXIT_REFUSED;
    }
    if let Some(t) = &rec.resolved_at {
        let _ = writeln!(
            err,
            "geoframe-export: そのジョブはすでに解除済みです（{t}）"
        );
        return EXIT_REFUSED;
    }
    let now = sys::now_rfc3339();
    rec.resolved_at = Some(now.clone());
    if store.write(&rec).is_err() {
        let _ = writeln!(
            err,
            "geoframe-export: 解除をジョブの記録に書けませんでした。何も変えていません"
        );
        return EXIT_REFUSED;
    }
    let _ = writeln!(
        out,
        "ジョブ {id} の blocked を解除しました（{now}）。Drive には触れていません。"
    );
    let _ = writeln!(
        out,
        "Drive の画面で、次に残っているものを確かめて片付けてください:"
    );
    if let Some(layout) = layout_of(settings, &rec) {
        let progress = rec.progress.as_ref();
        let id_text = |v: Option<&String>| v.map_or("記録なし".to_owned(), Clone::clone);
        let _ = writeln!(out, "  宛先: {}", layout.dest_path());
        let _ = writeln!(
            out,
            "  incoming: {}（記録した ID: {}）",
            layout.incoming_path(),
            id_text(progress.and_then(|p| p.incoming_id.as_ref()))
        );
        let _ = writeln!(
            out,
            "  replaced: {}（記録した旧フォルダの ID: {}）",
            layout.retired_path(),
            id_text(progress.and_then(|p| p.old_id.as_ref()))
        );
    }
    if let Some(b) = &rec.blocked
        && !b.description.is_empty()
    {
        let _ = writeln!(out, "止めたときの説明: {}", b.description);
    }
    EXIT_OK
}
