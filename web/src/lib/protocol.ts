export interface ImageInfo {
  width: number
  height: number
  /** 1 gray, 2 gray+alpha, 3 RGB, 4 RGBA */
  channels: number
  depth: number
  /** channels actually coded after removing constant alpha / gray colour */
  codedChannels: number
  source: 'png' | 'bmp'
  mode: number
  paletteSize: number
}

export type WorkerRequest =
  | { op: 'prepare'; file: ArrayBuffer }
  | { op: 'encodeStripe'; header: Uint8Array; rows: Uint8Array; height: number }
  | { op: 'parse'; file: Uint8Array }
  | { op: 'decodeStripe'; header: Uint8Array; data: Uint8Array; height: number }
  | { op: 'finish'; header: Uint8Array; coded: Uint8Array; wantPng: boolean }
  | { op: 'readImage'; file: ArrayBuffer }

export interface Candidate {
  header: Uint8Array
  stripes: [number, number][]
  info: ImageInfo
}

export interface Prepared {
  candidates: Candidate[]
  samples: Uint8Array
  coded: Uint8Array
}

export interface ParsedFile {
  headerLen: number
  stripes: { offset: number; length: number; y0: number; y1: number }[]
  info: ImageInfo
}

export type BaselineRequest =
  | { op: 'zip'; samples: Uint8Array; width: number; height: number; channels: number; depth: number }
  | { op: 'webp'; rgba: Uint8ClampedArray; width: number; height: number }
  | { op: 'jxl'; rgba: Uint8ClampedArray; width: number; height: number; effort: number }
