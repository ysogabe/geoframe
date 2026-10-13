"""同一場面グループの中の順位を、学習済みの線形モデルで付ける（任意）。

`GEOFRAME_RANK_MODEL`のJSON（既定は`GEOFRAME_DATA_DIR/models/rank_model.json`）があれば、
品質特徴（鮮明度・露出など）とembeddingから点数を出し、`quality.rank_group`の代わりに使う。
モデルは過去のGemini比較評価から学習したもの（学習はこのリポジトリの外で行う）。
ファイルが無い・形式が違う・embeddingのモデルが今と違う・グループにembeddingの無い写真が
ある場合は、従来どおり`rank_group`で順位を付ける。

点数 = Σ base_weights[k]·特徴[k] + embedding·u + c
（u = pca_componentsᵀ·pca_weights、c = −pca_mean·u。グループ内の比較にだけ使う）
"""
from __future__ import annotations

import json
import threading

from .config import RANK_MODEL_PATH

FORMAT = "geoframe-rank-linear-v1"
_lock = threading.Lock()
_cache = {"key": None, "model": None}


class RankModel:
    def __init__(self, raw: dict):
        if raw.get("format") != FORMAT:
            raise ValueError(f"形式が違います: {raw.get('format')!r}")
        self.name = str(raw["name"])
        self.embedding_model = str(raw["embedding_model"])
        self.embedding_dim = int(raw["embedding_dim"])
        self.base_features = [str(name) for name in raw["base_features"]]
        self.base_weights = [float(v) for v in raw["base_weights"]]
        components = raw["pca_components"]
        weights = [float(v) for v in raw["pca_weights"]]
        mean = [float(v) for v in raw["pca_mean"]]
        if (len(self.base_weights) != len(self.base_features) or len(components) != len(weights)
                or len(mean) != self.embedding_dim
                or any(len(row) != self.embedding_dim for row in components)):
            raise ValueError("係数の大きさが合いません")
        # 写真ごとに768次元の内積1回で済むよう、射影と重みを1本のベクトルにまとめる。
        self.u = [sum(w * row[i] for w, row in zip(weights, components))
                  for i in range(self.embedding_dim)]
        self.c = -sum(m * u for m, u in zip(mean, self.u))

    def score(self, metrics, embedding) -> float:
        base = sum(w * float(getattr(metrics, name))
                   for name, w in zip(self.base_features, self.base_weights))
        return base + sum(e * u for e, u in zip(embedding, self.u)) + self.c


def load_rank_model(embedding_model: str, embedding_dim: int):
    """使える学習済みモデルを返す。無ければNone。ファイルが変わったら読み直す。"""
    try:
        stat = RANK_MODEL_PATH.stat()
    except OSError:
        return None
    key = (str(RANK_MODEL_PATH), stat.st_mtime_ns, stat.st_size)
    with _lock:
        if _cache["key"] != key:
            try:
                _cache["model"] = RankModel(json.loads(RANK_MODEL_PATH.read_text(encoding="utf-8")))
                print(f"[ai] 順位付けモデルを読み込み: {_cache['model'].name}")
            except (OSError, ValueError, KeyError, TypeError) as exc:
                _cache["model"] = None
                print(f"[ai] 順位付けモデルを使いません（{RANK_MODEL_PATH}）: {exc}")
            _cache["key"] = key
        model = _cache["model"]
    if model is None or model.embedding_model != embedding_model or model.embedding_dim != embedding_dim:
        return None
    return model
