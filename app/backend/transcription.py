from __future__ import annotations

import ctypes
import json
import math
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


REMOTE_CHUNK_SECONDS = 600
REMOTE_AUDIO_BITRATE = "64k"
GROQ_TRANSCRIPTION_MODEL = "whisper-large-v3-turbo"


def _cuda_runtime_available() -> bool:
    try:
        import ctranslate2
        if ctranslate2.get_cuda_device_count() < 1:
            return False
        for library in ("libcublas.so.12", "libcudnn.so.9"):
            ctypes.CDLL(library)
    except (ImportError, OSError, RuntimeError):
        return False
    return True


def model_device_configuration(cuda_available: bool | None = None) -> tuple[str, str]:
    use_cuda = _cuda_runtime_available() if cuda_available is None else cuda_available
    return ("cuda", "int8_float16") if use_cuda else ("cpu", "int8")


def _multipart_body(fields: dict[str, str], filename: str, audio: bytes) -> tuple[bytes, str]:
    boundary = f"----PodcastStudio{uuid.uuid4().hex}"
    parts: list[bytes] = []
    for name, value in fields.items():
        parts.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode(),
                value.encode(),
                b"\r\n",
            ]
        )
    parts.extend(
        [
            f"--{boundary}\r\n".encode(),
            f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'.encode(),
            b"Content-Type: audio/mpeg\r\n\r\n",
            audio,
            b"\r\n",
            f"--{boundary}--\r\n".encode(),
        ]
    )
    return b"".join(parts), boundary


def _provider_request(
    provider: str,
    api_key: str,
    audio_path: Path,
    on_progress: Callable[[float, str], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
) -> dict[str, Any]:
    if provider == "assemblyai":
        return _assemblyai_transcription(audio_path, api_key, on_progress, is_cancelled)
    audio = audio_path.read_bytes()
    if provider == "groq":
        body, boundary = _multipart_body(
            {
                "model": GROQ_TRANSCRIPTION_MODEL,
                "response_format": "verbose_json",
                "timestamp_granularities[]": "word",
                "temperature": "0",
            },
            audio_path.name,
            audio,
        )
        request = Request(
            "https://api.groq.com/openai/v1/audio/transcriptions",
            data=body,
            headers={
                "Authorization": f"Bearer {api_key}",
                "Content-Type": f"multipart/form-data; boundary={boundary}",
            },
            method="POST",
        )
    elif provider == "deepgram":
        request = Request(
            "https://api.deepgram.com/v1/listen?model=nova-3&detect_language=true&smart_format=true",
            data=audio,
            headers={
                "Authorization": f"Token {api_key}",
                "Content-Type": "audio/mpeg",
            },
            method="POST",
        )
    else:
        raise ValueError(f"Unsupported transcription provider: {provider}")

    try:
        with urlopen(request, timeout=300) as response:
            return json.loads(response.read())
    except HTTPError as error:
        provider_detail = ""
        try:
            error_body = json.loads(error.read().decode("utf-8"))
            error_data = error_body.get("error", {})
            provider_detail = str(error_data.get("message", "")).strip()
        except (UnicodeDecodeError, json.JSONDecodeError, AttributeError, TypeError):
            pass
        if error.code == 401:
            message = "A chave da API foi recusada. Verifique a chave do provedor."
        elif error.code == 403 and provider == "groq":
            message = "A Groq negou a permissão para esta solicitação. Verifique as permissões da chave, do projeto e do modelo."
            if provider_detail:
                message = f"{message} Detalhe da Groq: {provider_detail}"
        elif error.code == 429:
            message = "O provedor atingiu o limite de uso ou a cota disponível."
        elif error.code == 413:
            message = "O provedor recusou o tamanho do áudio."
        elif error.code >= 500:
            message = "O serviço de transcrição está temporariamente indisponível."
        else:
            message = f"O provedor recusou a solicitação (HTTP {error.code})."
        raise RuntimeError(message) from error
    except URLError as error:
        raise RuntimeError("Não foi possível conectar ao serviço de transcrição.") from error
    except TimeoutError as error:
        raise RuntimeError("O serviço de transcrição excedeu o tempo limite.") from error


def _assemblyai_request(
    url: str,
    api_key: str,
    data: bytes | None = None,
    *,
    method: str = "GET",
    json_body: bool = False,
) -> dict[str, Any]:
    headers = {"Authorization": api_key}
    if json_body:
        headers["Content-Type"] = "application/json"
    elif data is not None:
        headers["Content-Type"] = "application/octet-stream"
    request = Request(url, data=data, headers=headers, method=method)
    try:
        with urlopen(request, timeout=300) as response:
            return json.loads(response.read())
    except HTTPError as error:
        detail = ""
        try:
            payload = json.loads(error.read().decode("utf-8"))
            detail = str(payload.get("error", payload.get("message", ""))).strip()
        except (UnicodeDecodeError, json.JSONDecodeError, AttributeError, TypeError):
            pass
        if error.code in {401, 403}:
            message = "A AssemblyAI recusou a chave ou a permissão da solicitação."
        elif error.code == 429:
            message = "A AssemblyAI atingiu o limite de solicitações ou de transcrições simultâneas."
        elif error.code == 413:
            message = "O arquivo excede o tamanho aceito pela AssemblyAI."
        elif error.code >= 500:
            message = "O serviço da AssemblyAI está temporariamente indisponível."
        else:
            message = f"A AssemblyAI recusou a solicitação (HTTP {error.code})."
        if detail:
            message = f"{message} Detalhe da AssemblyAI: {detail}"
        raise RuntimeError(message) from error
    except URLError as error:
        raise RuntimeError("Não foi possível conectar à AssemblyAI.") from error
    except TimeoutError as error:
        raise RuntimeError("A solicitação à AssemblyAI excedeu o tempo limite.") from error


def _assemblyai_transcription(
    audio_path: Path,
    api_key: str,
    on_progress: Callable[[float, str], None] | None = None,
    is_cancelled: Callable[[], bool] | None = None,
) -> dict[str, Any]:
    if is_cancelled and is_cancelled():
        raise InterruptedError("Transcription cancelled")
    on_progress and on_progress(0.08, "Enviando áudio para AssemblyAI")
    upload = _assemblyai_request(
        "https://api.assemblyai.com/v2/upload",
        api_key,
        audio_path.read_bytes(),
        method="POST",
    )
    upload_url = upload.get("upload_url")
    if not upload_url:
        raise RuntimeError("A AssemblyAI não retornou o endereço do áudio enviado.")
    on_progress and on_progress(0.22, "Solicitando transcrição à AssemblyAI")
    submission = _assemblyai_request(
        "https://api.assemblyai.com/v2/transcript",
        api_key,
        json.dumps({
            "audio_url": upload_url,
            "speech_models": ["universal-3-5-pro", "universal-2"],
            "language_detection": True,
        }).encode("utf-8"),
        method="POST",
        json_body=True,
    )
    transcript_id = submission.get("id")
    if not transcript_id:
        raise RuntimeError("A AssemblyAI não retornou o identificador da transcrição.")
    on_progress and on_progress(0.35, "Aguardando a transcrição da AssemblyAI")
    while True:
        if is_cancelled and is_cancelled():
            raise InterruptedError("Transcription cancelled")
        time.sleep(2)
        result = _assemblyai_request(
            f"https://api.assemblyai.com/v2/transcript/{transcript_id}",
            api_key,
        )
        status = result.get("status")
        if status == "completed":
            return result
        if status == "error":
            detail = str(result.get("error", "A transcrição falhou.")).strip()
            raise RuntimeError(f"A AssemblyAI não concluiu a transcrição: {detail}")
        if is_cancelled and is_cancelled():
            raise InterruptedError("Transcription cancelled")
        on_progress and on_progress(0.55, "A AssemblyAI ainda está processando o áudio")


def _provider_words(provider: str, result: dict[str, Any]) -> tuple[list[tuple[str, float, float]], str | None]:
    if provider == "groq":
        raw_words = result.get("words") or []
        language = result.get("language")
        words = [
            (str(word.get("word", "")).strip(), float(word["start"]), float(word["end"]))
            for word in raw_words
            if word.get("word") and word.get("start") is not None and word.get("end") is not None
        ]
    elif provider == "deepgram":
        try:
            channel = result["results"]["channels"][0]
            alternative = channel["alternatives"][0]
        except (KeyError, IndexError, TypeError) as error:
            raise RuntimeError("O Deepgram retornou uma resposta de transcrição inválida.") from error
        language = channel.get("detected_language")
        words = [
            (str(word.get("punctuated_word") or word.get("word", "")).strip(), float(word["start"]), float(word["end"]))
            for word in alternative.get("words", [])
            if (word.get("punctuated_word") or word.get("word"))
            and word.get("start") is not None
            and word.get("end") is not None
        ]
    else:
        raw_words = result.get("words") or []
        language = result.get("language_code")
        words = [
            (str(word.get("text", "")).strip(), float(word["start"]) / 1000, float(word["end"]) / 1000)
            for word in raw_words
            if word.get("text") and word.get("start") is not None and word.get("end") is not None
        ]
    return [(text, start, end) for text, start, end in words if text and end > start], language


def transcribe_remote(
    media_path: str | Path,
    duration: float,
    provider: str,
    api_key: str,
    on_progress: Callable[[float, str], None],
    is_cancelled: Callable[[], bool],
) -> tuple[list[dict[str, Any]], str | None]:
    if provider not in {"groq", "deepgram", "assemblyai"}:
        raise ValueError(f"Unsupported transcription provider: {provider}")
    chunk_count = max(1, math.ceil(duration / REMOTE_CHUNK_SECONDS))
    words: list[dict[str, Any]] = []
    language: str | None = None

    with tempfile.TemporaryDirectory(prefix="podcast-transcription-") as directory:
        temp_dir = Path(directory)
        for index in range(chunk_count):
            if is_cancelled():
                raise InterruptedError("Transcription cancelled")
            offset = index * REMOTE_CHUNK_SECONDS
            chunk_duration = min(REMOTE_CHUNK_SECONDS, max(0.1, duration - offset))
            audio_path = temp_dir / f"chunk-{index:04d}.mp3"
            command = [
                "ffmpeg", "-y", "-v", "error", "-ss", str(offset),
                "-i", str(media_path), "-t", str(chunk_duration), "-vn",
                "-ac", "1", "-ar", "16000", "-c:a", "libmp3lame",
                "-b:a", REMOTE_AUDIO_BITRATE, str(audio_path),
            ]
            try:
                subprocess.run(command, check=True, capture_output=True, timeout=180)
            except subprocess.CalledProcessError as error:
                raise RuntimeError("Não foi possível preparar o áudio para a transcrição.") from error
            except subprocess.TimeoutExpired as error:
                raise RuntimeError("A preparação do áudio excedeu o tempo limite.") from error
            if is_cancelled():
                raise InterruptedError("Transcription cancelled")

            on_progress(
                0.1 + 0.8 * (index / chunk_count),
                f"Enviando trecho {index + 1} de {chunk_count} para {provider.title()}",
            )
            chunk_start_progress = 0.1 + 0.8 * (index / chunk_count)
            chunk_end_progress = 0.1 + 0.8 * ((index + 1) / chunk_count)
            def report_provider_progress(fraction: float, message: str) -> None:
                on_progress(
                    chunk_start_progress + (chunk_end_progress - chunk_start_progress) * fraction,
                    message,
                )

            result = _provider_request(
                provider,
                api_key,
                audio_path,
                report_provider_progress if provider == "assemblyai" else None,
                is_cancelled,
            )
            chunk_words, detected_language = _provider_words(provider, result)
            if is_cancelled():
                raise InterruptedError("Transcription cancelled")
            if detected_language:
                language = str(detected_language)
            words.extend(
                {
                    "id": uuid.uuid4().hex,
                    "text": text,
                    "start": round(start + offset, 3),
                    "end": round(end + offset, 3),
                    "speaker": "Speaker 1",
                }
                for text, start, end in chunk_words
            )
            on_progress(0.1 + 0.8 * ((index + 1) / chunk_count), "Recebendo transcrição")

    if is_cancelled():
        raise InterruptedError("Transcription cancelled")
    return words, language
