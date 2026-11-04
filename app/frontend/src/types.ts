export type Media = {
  id: string;
  original_filename: string;
  duration: number;
  has_video: boolean;
  has_audio: boolean;
  width: number;
  height: number;
  fps: number;
  mime_type: string;
};

export type MediaSequenceItem = {
  project_id: string;
  media_id: string;
  name: string;
  duration: number;
  has_video: boolean;
};

export type Word = {
  id: string;
  text: string;
  start: number;
  end: number;
  speaker: string;
};

export type TopicMarker = {
  id: string;
  title: string;
  time: number;
};

export type Interval = { start: number; end: number };

export type Project = {
  id: string;
  name: string;
  media_id: string;
  transcript: Word[];
  topic_markers?: TopicMarker[];
  edl: { duration: number; kept: Interval[]; edited_duration: number };
  undo_stack: unknown[];
  redo_stack: unknown[];
  enhancement: Enhancement;
  language?: string;
};

export type Enhancement = {
  input_gain_db: number;
  highpass_hz: number;
  lowpass_hz: number;
  noise_reduction: number;
  speech_gate: number;
  deesser_mix: number;
  presence_eq_db: number;
  compressor_amount: number;
  loudnorm_target: number;
  output_gain_db: number;
};

export type Peak = { min: number; max: number };
