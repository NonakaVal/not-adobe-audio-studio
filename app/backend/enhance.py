from __future__ import annotations

from pydantic import BaseModel, Field


class EnhancementParams(BaseModel):
    input_gain_db: float = Field(default=0, ge=-18, le=12)
    highpass_hz: int = Field(default=70, ge=20, le=400)
    lowpass_hz: int = Field(default=18000, ge=8000, le=20000)
    noise_reduction: int = Field(default=30, ge=0, le=100)
    speech_gate: int = Field(default=0, ge=0, le=100)
    deesser_mix: int = Field(default=0, ge=0, le=100)
    presence_eq_db: float = Field(default=0, ge=-8, le=8)
    compressor_amount: int = Field(default=20, ge=0, le=100)
    loudnorm_target: float = Field(default=-16, ge=-34, le=-12)
    output_gain_db: float = Field(default=0, ge=-12, le=12)


PRESETS: dict[str, dict[str, object]] = {
    "Natural": {},
    "Podcast": {"noise_reduction": 40, "speech_gate": 12, "compressor_amount": 32, "loudnorm_target": -16},
    "Interview": {"noise_reduction": 55, "highpass_hz": 90, "compressor_amount": 38, "loudnorm_target": -17},
    "Studio": {"noise_reduction": 20, "presence_eq_db": 2, "compressor_amount": 45, "loudnorm_target": -14},
    "Locução": {"noise_reduction": 25, "highpass_hz": 80, "presence_eq_db": 2, "deesser_mix": 12, "compressor_amount": 38, "loudnorm_target": -16},
    "Chamada remota": {"noise_reduction": 58, "highpass_hz": 110, "lowpass_hz": 12000, "speech_gate": 18, "deesser_mix": 10, "compressor_amount": 42, "loudnorm_target": -16},
    "Ambiente ruidoso": {"noise_reduction": 70, "highpass_hz": 100, "speech_gate": 22, "compressor_amount": 35, "loudnorm_target": -16},
}


def build_filter_chain(params: EnhancementParams) -> str:
    filters = [f"volume={params.input_gain_db:g}dB"]
    filters.extend((f"highpass=f={params.highpass_hz}", f"lowpass=f={params.lowpass_hz}"))
    if params.noise_reduction:
        noise_floor = -20 - (params.noise_reduction * 0.4)
        filters.append(f"afftdn=nf={noise_floor:g}:tn=1")
    if params.presence_eq_db:
        filters.append(f"equalizer=f=3200:t=q:w=1:g={params.presence_eq_db:g}")
    if params.deesser_mix:
        filters.append(f"deesser=i={params.deesser_mix / 100:.2f}")
    if params.compressor_amount:
        ratio = 1 + params.compressor_amount / 12
        filters.append(f"acompressor=threshold=-18dB:ratio={ratio:.2f}:attack=20:release=200")
    if params.speech_gate:
        threshold = -60 + params.speech_gate * 0.35
        filters.append(f"agate=threshold={threshold:g}dB:ratio=4:attack=10:release=150")
    filters.append(f"loudnorm=I={params.loudnorm_target:g}:TP=-1.5:LRA=11")
    filters.append(f"volume={params.output_gain_db:g}dB")
    return ",".join(filters)
