"""Google Drive 連携とローカルフォルダ索引（server.py L408-L574 / L1381-L1396 より移動）"""
import hashlib
import re
from datetime import datetime

from .config import (DATA_DIR, DERIV_DIR, DRIVE_FOLDER, DRIVE_KEY, DRIVE_MIMES,
                     EXTS, PHOTOS_DIR, TMP_DIR)
from .geo import infer_locations
from .store import dbq, dbx
from .util import derivative_name, extract_exif, make_derivative, norm_time


# ============================================================
# Drive 連携
# ============================================================
_drive_folder_id = None


def drive_service():
    from google.oauth2 import service_account
    from googleapiclient.discovery import build
    creds = service_account.Credentials.from_service_account_file(
        str(DRIVE_KEY), scopes=["https://www.googleapis.com/auth/drive.readonly"])
    return build("drive", "v3", credentials=creds, cache_discovery=False)


def drive_resolve_folder(svc):
    global _drive_folder_id
    if _drive_folder_id:
        return _drive_folder_id
    if re.fullmatch(r"[A-Za-z0-9_-]{20,}", DRIVE_FOLDER):
        _drive_folder_id = DRIVE_FOLDER
        return _drive_folder_id
    q = (f"name='{DRIVE_FOLDER}' and "
         "mimeType='application/vnd.google-apps.folder' and trashed=false")
    files = svc.files().list(q=q, fields="files(id)", pageSize=5).execute().get("files", [])
    if not files:
        raise RuntimeError(f"Driveにフォルダ '{DRIVE_FOLDER}' が見つかりません（共有を確認）")
    _drive_folder_id = files[0]["id"]
    return _drive_folder_id


def index_drive(svc):
    """Drive一覧のimageMediaMetadataでダウンロード無しに索引化"""
    fid = drive_resolve_folder(svc)
    remote, token = {}, None
    while True:
        res = svc.files().list(
            q=f"'{fid}' in parents and trashed=false",
            fields=("nextPageToken,files(id,name,md5Checksum,mimeType,"
                    "imageMediaMetadata(time,location))"),
            pageSize=1000, pageToken=token).execute()
        for f in res.get("files", []):
            if f.get("mimeType") in DRIVE_MIMES:
                remote[f["id"]] = f
        token = res.get("nextPageToken")
        if not token:
            break
    now = datetime.now().isoformat(timespec="seconds")
    n_new = 0
    for gid, f in remote.items():
        meta = f.get("imageMediaMetadata") or {}
        loc = meta.get("location") or {}
        lat = loc.get("latitude")
        lon = loc.get("longitude")
        taken = norm_time(meta.get("time"))
        row = dbq("SELECT id, md5 FROM photos WHERE drive_id=?", (gid,))
        if row and row[0]["md5"] == f.get("md5Checksum"):
            continue
        if row:  # 内容更新 → derivative作り直し（Drive側で向きを直した場合に二重に回さないよう回転も戻す）
            dbx("""UPDATE photos SET name=?, md5=?, taken_at=?,
                   lat=?, lon=?, loc_source=?, place=NULL, elev=NULL,
                   display=NULL, fetched_at=NULL, rotation=0, indexed_at=? WHERE drive_id=?""",
                (f["name"], f.get("md5Checksum"), taken, lat, lon,
                 "drive_meta" if lat is not None else None, now, gid))
        else:
            dbx("""INSERT INTO photos(source, drive_id, name, md5, taken_at,
                   lat, lon, loc_source, indexed_at)
                   VALUES('drive',?,?,?,?,?,?,?,?)""",
                (gid, f["name"], f.get("md5Checksum"), taken, lat, lon,
                 "drive_meta" if lat is not None else None, now))
            n_new += 1
    # Drive側で消えた行を削除（derivativeも掃除）
    n_del = 0
    for r in dbq("SELECT id, drive_id, display FROM photos WHERE source='drive'"):
        if r["drive_id"] not in remote:
            if r["display"]:
                (DATA_DIR / r["display"]).unlink(missing_ok=True)
            dbx("DELETE FROM photos WHERE id=?", (r["id"],))
            n_del += 1
    return len(remote), n_new, n_del


def prepare_tmp_dir():
    """一時ダウンロード用ディレクトリを用意し、前回の残骸を掃除する（起動時に1回）。
    旧実装が derivatives/ 配下に置いていた .dl_* も対象にする。
    ※残骸を消すのは、フル解像度・EXIF完全保持のDrive原本（GPS座標・カメラ機体
      シリアル・原撮影時刻を含む）をディスクに置いたままにしないため。
      derivatives/ 側は静的配信の許可prefix配下なので特に放置してはいけない"""
    TMP_DIR.mkdir(parents=True, exist_ok=True)
    n = 0
    for d in (TMP_DIR, DERIV_DIR):
        if not d.is_dir():
            continue
        for p in d.glob(".dl_*"):
            try:
                p.unlink()
                n += 1
            except OSError:
                pass
    if n:
        print(f"[fetch] 一時ダウンロードの残骸を{n}件削除しました")
    return n


def fetch_derivatives(svc, limit):
    """derivative未生成のDrive写真を取得・変換。
    位置の有無では絞らない。/admin の「位置なし」タブは写真を見ながら手動で
    位置を指定するための画面であり、サムネイルが無いと何の写真か分からず
    指定のしようがないため（位置ありに限ると、位置なし写真は永久に
    derivativeが作られず「画像未取得」のままになる）。
    取得順の taken_at は /admin の一覧と同じ並びなので、先頭ページから埋まる。"""
    from googleapiclient.http import MediaIoBaseDownload
    rows = dbq("""SELECT id, drive_id, name, rotation FROM photos
                  WHERE source='drive' AND display IS NULL
                  ORDER BY taken_at LIMIT ?""", (limit,))
    for r in rows:
        # 原本の一時置き場は derivatives/ の外（TMP_DIR）。詳細は config.TMP_DIR のコメント
        tmp = TMP_DIR / f".dl_{r['drive_id']}"
        try:
            TMP_DIR.mkdir(parents=True, exist_ok=True)
            DERIV_DIR.mkdir(exist_ok=True)
            with open(tmp, "wb") as out:
                dl = MediaIoBaseDownload(out, svc.files().get_media(fileId=r["drive_id"]))
                done = False
                while not done:
                    _, done = dl.next_chunk()
            dest = DERIV_DIR / derivative_name(r["drive_id"], r["rotation"] or 0)
            make_derivative(tmp, dest, r["rotation"] or 0)
            dbx("UPDATE photos SET display=?, fetched_at=? WHERE id=?",
                (dest.relative_to(DATA_DIR).as_posix(),
                 datetime.now().isoformat(timespec="seconds"), r["id"]))
            print(f"  [fetch] {r['name']} → derivative生成")
        except Exception as e:
            print(f"  [fetch] {r['name']} 失敗: {e}")
        finally:
            # 変換に失敗した場合もEXIF付き原本を残さない（従来は成功時のみ削除していた）
            tmp.unlink(missing_ok=True)
    return len(rows)


# ============================================================
# ローカルフォルダ索引
# ============================================================
def index_local():
    PHOTOS_DIR.mkdir(exist_ok=True)
    seen = set()
    now = datetime.now().isoformat(timespec="seconds")
    for p in sorted(PHOTOS_DIR.rglob("*")):
        if not p.is_file() or p.suffix.lower() not in EXTS:
            continue
        if "gdrive" in p.relative_to(PHOTOS_DIR).parts:
            continue  # 旧ミラー先はDrive行と重複するため除外
        rel = p.relative_to(DATA_DIR).as_posix()
        seen.add(rel)
        mtime = p.stat().st_mtime
        row = dbq("SELECT id, mtime FROM photos WHERE path=?", (rel,))
        if row and abs(row[0]["mtime"] - mtime) < 1:
            continue
        try:
            lat, lon, taken = extract_exif(p)
        except Exception as e:
            print(f"  [local] {rel}: 読み込み失敗 ({e})")
            continue
        loc_source = "exif" if lat is not None else None
        if row:
            dbx("""UPDATE photos SET mtime=?, taken_at=?, lat=?, lon=?, loc_source=?,
                   place=NULL, elev=NULL, display=NULL, indexed_at=? WHERE path=?""",
                (mtime, taken, lat, lon, loc_source, now, rel))
        else:
            dbx("""INSERT INTO photos(source, path, name, mtime, taken_at,
                   lat, lon, loc_source, indexed_at)
                   VALUES('local',?,?,?,?,?,?,?,?)""",
                (rel, p.name, mtime, taken, lat, lon, loc_source, now))
    for r in dbq("SELECT id, path, display FROM photos WHERE source='local'"):
        if r["path"] not in seen:
            if r["display"] and r["display"].startswith("derivatives/"):
                (DATA_DIR / r["display"]).unlink(missing_ok=True)
            dbx("DELETE FROM photos WHERE id=?", (r["id"],))


def local_derivatives(limit):
    """ローカル写真の表示用ファイルを準備（JPG/PNGは原本、HEIC等はderivative）"""
    rows = dbq("""SELECT id, path, name FROM photos
                  WHERE source='local' AND lat IS NOT NULL AND display IS NULL
                  LIMIT ?""", (limit,))
    for r in rows:
        src = DATA_DIR / r["path"]
        try:
            if src.suffix.lower() in (".jpg", ".jpeg", ".png"):
                display = r["path"]          # ブラウザ表示可能なのでそのまま
            else:
                h = hashlib.md5(r["path"].encode()).hexdigest()[:16]
                dest = DERIV_DIR / f"local_{h}.jpg"
                make_derivative(src, dest)
                display = dest.relative_to(DATA_DIR).as_posix()
            dbx("UPDATE photos SET display=?, fetched_at=? WHERE id=?",
                (display, datetime.now().isoformat(timespec="seconds"), r["id"]))
        except Exception as e:
            print(f"  [local] {r['name']} derivative失敗: {e}")


def organize_photos():
    index_local()
    infer_locations()
    rows = dbq("SELECT path, name FROM photos WHERE source='local' AND lat IS NULL")
    if not rows:
        print("移動対象はありません。")
        return
    dest = DATA_DIR / "_excluded" / "no_gps"
    dest.mkdir(parents=True, exist_ok=True)
    for r in rows:
        src = DATA_DIR / r["path"]
        if src.exists():
            src.rename(dest / src.name)
            print(f"  移動: {r['path']} → _excluded/no_gps/")
        dbx("DELETE FROM photos WHERE path=?", (r["path"],))
    print(f"{len(rows)}件を退避しました。")
