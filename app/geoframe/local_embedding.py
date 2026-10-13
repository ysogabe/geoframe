"""embeddinggemma-2を同じプロセスのCPUで動かすembeddingクライアント。

GEOFRAME_EMBED_BACKEND=local のとき、Gemini Embedding 2の代わりに使う。画像と検索語は
同じ768次元の空間に入るので、検索と同一場面のまとめはGemini版と同じ使い方になる。

- 依存（torch・sentence-transformers）は requirements-local-ai.txt で任意に入れる。
  入っていなければ AiSearchUnavailable にして、写真を消さずに後で再試行する。
- モデルはネットワークから取得せず、LOCAL_EMBED_MODEL_DIR から読む。
- ai_worker と検索APIの両スレッドから呼ばれるため、モデルは1つだけ載せ、encodeを直列にする。
"""
from __future__ import annotations

import base64
import io
import os
import threading

from PIL import Image

from .config import DATA_DIR, LOCAL_EMBED_MODEL_DIR, LOCAL_EMBED_THREADS

LOCAL_EMBED_MODEL = "embeddinggemma-2"
LOCAL_EMBED_DIM = 768
# 検索語に付けるプロンプト（sentence-transformersの設定名）。画像側は付けない
QUERY_PROMPT_NAME = "SearchQuery"


class LocalEmbeddingClient:
    _shared = None
    _shared_lock = threading.Lock()

    def __init__(self, model_dir=LOCAL_EMBED_MODEL_DIR, threads=LOCAL_EMBED_THREADS):
        self.model_dir = model_dir
        self.threads = threads
        self._model = None
        self._lock = threading.Lock()

    @classmethod
    def shared(cls):
        with cls._shared_lock:
            if cls._shared is None:
                cls._shared = cls()
            return cls._shared

    def _load(self):
        # 呼び出し側（ai_search）の例外型を使う。循環importを避けるためここで読む。
        from .ai_search import AiSearchUnavailable
        if not self.model_dir.is_dir():
            raise AiSearchUnavailable(f"ローカルembeddingのモデルがありません: {self.model_dir}")
        # サービスはホームに書けず（ProtectHome）、ネットワークからも取得しない。
        # 運用者が明示した値は残す。
        os.environ.setdefault("HF_HUB_OFFLINE", "1")
        os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
        os.environ.setdefault("HF_HOME", str(DATA_DIR / "cache" / "huggingface"))
        try:
            import torch
            from sentence_transformers import SentenceTransformer
        except ImportError as exc:
            raise AiSearchUnavailable(
                "ローカルembeddingの依存がありません（requirements-local-ai.txt）") from exc
        torch.set_num_threads(self.threads)
        # audio_configを外すと、画像と文だけの読み込みになり音声側の重みを載せない。
        try:
            self._model = SentenceTransformer(
                str(self.model_dir), device="cpu", local_files_only=True,
                model_kwargs={"dtype": torch.bfloat16}, config_kwargs={"audio_config": None})
        except Exception as exc:
            raise AiSearchUnavailable(
                f"ローカルembeddingのモデルを読み込めません: {self.model_dir}: {exc}") from exc
        print(f"[ai] ローカルembeddingを読み込み: {self.model_dir}（CPU {self.threads}スレッド）")

    def _encode(self, items, **kwargs) -> list[float]:
        from .ai_search import AiSearchUnavailable
        with self._lock:
            if self._model is None:
                self._load()
            try:
                vector = self._model.encode(items, normalize_embeddings=True,
                                            convert_to_numpy=True, **kwargs)[0]
            except Exception as exc:
                raise AiSearchUnavailable(f"ローカルembeddingに失敗しました: {exc}") from exc
        # 別のモデルを指定された場合に、長さの違うベクトルを保存して毎サイクル作り直すのを防ぐ。
        if len(vector) != LOCAL_EMBED_DIM:
            raise AiSearchUnavailable(f"ローカルembeddingの次元数が不正です: {len(vector)}")
        return [float(v) for v in vector]

    def embed_part(self, part: dict) -> list[float]:
        """AgentPlatformEmbeddingClient.embed_part と同じ形の入力を受ける。"""
        if "text" in part:
            return self.embed_text(part["text"])
        data = base64.b64decode(part["inlineData"]["data"])
        with Image.open(io.BytesIO(data)) as image:
            image = image.convert("RGB")
        return self._encode([{"image": image}])

    def embed_text(self, text: str) -> list[float]:
        return self._encode([text], prompt_name=QUERY_PROMPT_NAME)
